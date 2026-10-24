//! Scoring turn detectors against recorded conversations.
//!
//! Every voice framework ships a turn detector and none of them ship a way to
//! find out whether it works on your calls. You adopt a model on faith and
//! tune it by ear. This is the other half: replay real recorded sessions
//! through any [`TurnDetector`] and score where it *would* have committed
//! against where the user *actually* stopped.
//!
//! ## Why the ground truth is trustworthy
//!
//! A live detector cannot see the future — that is the entire difficulty of
//! the problem. An offline scorer can. Given a whole recording it can set its
//! threshold from the file's own noise floor, look forward to decide whether a
//! gap is a pause or an ending, and revise a boundary after the fact. None of
//! that is available to a detector running in a session, which is precisely
//! why it makes a fair reference for one.
//!
//! So the oracle here is deliberately non-causal ([`speech_spans`]), and needs
//! no hand labelling: the input is `session_audio.wav` as written by
//! `[pipeline.audit] record_session_audio`, channel 0 the user.
//!
//! ## What a score means
//!
//! For each user turn the oracle found, the replay records when the detector
//! first said the turn was over, and the difference is the number that
//! matters:
//!
//! * **positive** — the detector waited that many ms past the real end. This
//!   is dead air the caller hears.
//! * **negative** — it committed while the user was still speaking. This is a
//!   cut-off, and is not simply "better latency": it is the failure the
//!   latency is traded against, which is why [`TurnScore`] keeps the sign and
//!   [`DetectorScore`] reports both rather than a single mean.
//!
//! A detector that never fires is not scored as infinitely slow; it is counted
//! separately as a hang, because it degrades to the silence timeout rather
//! than to nothing.

use std::time::Duration;

use voice_pipes::stt::TranscriptionResult;
use voice_pipes::stt::turn::TurnDetector;

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;

/// A stretch of speech on one channel, in milliseconds from the recording's
/// start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechSpan {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl SpeechSpan {
    pub const fn duration_ms(self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// How the offline oracle decides what counts as speech and as a turn.
///
/// These are deliberately not the live VAD's settings: this one is allowed to
/// be slow and to look at the whole file, so it can afford a longer gap before
/// it calls a turn over.
#[derive(Debug, Clone, Copy)]
pub struct OracleConfig {
    /// Analysis frame, in milliseconds.
    pub frame_ms: u64,
    /// A frame counts as speech at this multiple of the estimated noise floor.
    pub speech_over_floor: f32,
    /// Absolute floor, so a silent recording does not amplify its own noise
    /// into "speech".
    pub min_rms: f32,
    /// Gaps shorter than this join two spans: a breath inside a sentence is
    /// not the end of a turn.
    pub merge_gap_ms: u64,
    /// Spans shorter than this are dropped as clicks or lip noise.
    pub min_span_ms: u64,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            frame_ms: 20,
            speech_over_floor: 3.0,
            min_rms: 0.005,
            // Long on purpose. Conversational pauses inside a turn routinely
            // reach half a second; a shorter value would split one turn into
            // several and flatter every detector that commits early.
            merge_gap_ms: 600,
            min_span_ms: 200,
        }
    }
}

/// Root-mean-square of one frame.
fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
}

/// Estimate the noise floor as a low percentile of frame energy.
///
/// The mean would be dragged up by the speech itself; a percentile of the
/// quiet frames is what "quiet" sounds like in *this* recording, which is the
/// point of doing this offline.
fn noise_floor(frame_rms: &[f32]) -> f32 {
    if frame_rms.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = frame_rms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 10]
}

/// Find the speech in one channel, using the whole signal.
///
/// Non-causal by design: the threshold comes from the file's own noise floor
/// and gaps are judged with full lookahead. A live detector can do neither,
/// which is what makes this usable as a reference for one.
pub fn speech_spans(samples: &[f32], sample_rate: u32, cfg: OracleConfig) -> Vec<SpeechSpan> {
    if samples.is_empty() || sample_rate == 0 || cfg.frame_ms == 0 {
        return Vec::new();
    }
    let frame_len = (sample_rate as u64 * cfg.frame_ms / 1000).max(1) as usize;
    let frames: Vec<f32> = samples.chunks(frame_len).map(rms).collect();
    let floor = noise_floor(&frames);
    let threshold = (floor * cfg.speech_over_floor).max(cfg.min_rms);

    let ms_per_frame = cfg.frame_ms;
    let mut spans: Vec<SpeechSpan> = Vec::new();
    let mut open: Option<u64> = None;
    for (i, &level) in frames.iter().enumerate() {
        let at = i as u64 * ms_per_frame;
        if level >= threshold {
            open.get_or_insert(at);
        } else if let Some(start) = open.take() {
            spans.push(SpeechSpan {
                start_ms: start,
                end_ms: at,
            });
        }
    }
    if let Some(start) = open {
        spans.push(SpeechSpan {
            start_ms: start,
            end_ms: frames.len() as u64 * ms_per_frame,
        });
    }

    merge_and_filter(spans, cfg)
}

/// Join spans separated by less than `merge_gap_ms`, then drop the short ones.
///
/// Order matters: merging first means a burst of syllables separated by
/// consonant gaps becomes one span and survives the length filter, rather than
/// being discarded as a handful of too-short fragments.
fn merge_and_filter(spans: Vec<SpeechSpan>, cfg: OracleConfig) -> Vec<SpeechSpan> {
    let mut merged: Vec<SpeechSpan> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(prev) if span.start_ms.saturating_sub(prev.end_ms) <= cfg.merge_gap_ms => {
                prev.end_ms = span.end_ms;
            }
            _ => merged.push(span),
        }
    }
    merged.retain(|s| s.duration_ms() >= cfg.min_span_ms);
    merged
}

/// When a detector said the turn was over, during replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    pub at_ms: u64,
    /// The detector's own silence fallback fired rather than its model.
    /// Counted separately: a mode that only ever reaches this is behaving as
    /// `silence`, whatever it is configured as.
    pub forced: bool,
}

/// One user turn, and what the detector did about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnScore {
    /// Where the oracle says the user stopped.
    pub turn_end_ms: u64,
    pub commit_ms: Option<u64>,
    /// Commit minus true end. Negative means the detector cut the user off.
    pub endpoint_latency_ms: Option<i64>,
    pub cut_off: bool,
    pub forced: bool,
    /// No commit at all before the next turn began.
    pub hung: bool,
}

/// Pair each oracle turn with the detector's first commit after that turn
/// started, and score it.
///
/// A commit is attributed to the turn it falls within or follows, up to the
/// start of the next turn. Anything the detector emits before the first turn
/// begins is ignored rather than attributed backwards.
pub fn score_turns(turns: &[SpeechSpan], commits: &[Commit]) -> Vec<TurnScore> {
    let mut scores = Vec::with_capacity(turns.len());
    for (i, turn) in turns.iter().enumerate() {
        let window_end = turns.get(i + 1).map_or(u64::MAX, |next| next.start_ms);
        let commit = commits
            .iter()
            .find(|c| c.at_ms >= turn.start_ms && c.at_ms < window_end);
        match commit {
            Some(c) => scores.push(TurnScore {
                turn_end_ms: turn.end_ms,
                commit_ms: Some(c.at_ms),
                endpoint_latency_ms: Some(c.at_ms as i64 - turn.end_ms as i64),
                cut_off: c.at_ms < turn.end_ms,
                forced: c.forced,
                hung: false,
            }),
            None => scores.push(TurnScore {
                turn_end_ms: turn.end_ms,
                commit_ms: None,
                endpoint_latency_ms: None,
                cut_off: false,
                forced: false,
                hung: true,
            }),
        }
    }
    scores
}

/// One detector's behaviour over a corpus.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DetectorScore {
    pub turns: usize,
    /// Median over turns the detector committed *after* the real end. Cut-offs
    /// are excluded: averaging a negative latency against a positive one
    /// reads as "fast" when it is really "sometimes wrong".
    pub median_endpoint_latency_ms: Option<i64>,
    pub p90_endpoint_latency_ms: Option<i64>,
    /// Share of turns committed while the user was still speaking.
    pub cut_off_rate: f64,
    /// Share of turns that reached the silence fallback.
    pub forced_rate: f64,
    /// Share of turns with no commit at all.
    pub hang_rate: f64,
}

/// Aggregate per-turn scores.
pub fn aggregate(scores: &[TurnScore]) -> DetectorScore {
    if scores.is_empty() {
        return DetectorScore::default();
    }
    let n = scores.len() as f64;
    let mut late: Vec<i64> = scores
        .iter()
        .filter(|s| !s.cut_off)
        .filter_map(|s| s.endpoint_latency_ms)
        .collect();
    late.sort_unstable();

    let percentile = |v: &[i64], p: f64| -> Option<i64> {
        if v.is_empty() {
            return None;
        }
        let idx = (((v.len() - 1) as f64) * p).round() as usize;
        Some(v[idx])
    };

    DetectorScore {
        turns: scores.len(),
        median_endpoint_latency_ms: percentile(&late, 0.5),
        p90_endpoint_latency_ms: percentile(&late, 0.9),
        cut_off_rate: scores.iter().filter(|s| s.cut_off).count() as f64 / n,
        forced_rate: scores.iter().filter(|s| s.forced).count() as f64 / n,
        hang_rate: scores.iter().filter(|s| s.hung).count() as f64 / n,
    }
}

/// Render a comparison table, one row per detector.
pub fn render_table(rows: &[(String, DetectorScore)]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    // Writing into a `String` cannot fail.
    let _ = writeln!(
        out,
        "{:<18} {:>6} {:>10} {:>10} {:>9} {:>8} {:>7}",
        "detector", "turns", "median ms", "p90 ms", "cut-off", "forced", "hang"
    );
    out.push_str(&"-".repeat(74));
    out.push('\n');
    for (name, s) in rows {
        let fmt = |v: Option<i64>| v.map_or_else(|| "-".to_string(), |x| x.to_string());
        let _ = writeln!(
            out,
            "{:<18} {:>6} {:>10} {:>10} {:>8.1}% {:>7.1}% {:>6.1}%",
            name,
            s.turns,
            fmt(s.median_endpoint_latency_ms),
            fmt(s.p90_endpoint_latency_ms),
            s.cut_off_rate * 100.0,
            s.forced_rate * 100.0,
            s.hang_rate * 100.0,
        );
    }
    out
}

/// A transcript stand-in for replay.
///
/// The detectors under test key off audio and elapsed silence, but
/// [`TurnDetector::evaluate`] takes an STT result, and at least one of them
/// declines to run on an empty transcript. Replay has no recogniser, so it
/// supplies a non-empty placeholder: the text is never read for a decision,
/// only checked for emptiness.
///
/// The consequence is worth stating plainly rather than discovering from odd
/// numbers: `provider` mode cannot be scored this way, because its whole input
/// is the `is_end_of_turn` flag a real provider would set. Score `silence`,
/// `smart` and `vap`.
pub fn replay_transcript() -> TranscriptionResult {
    TranscriptionResult::new("replay", false).with_speech_started(true)
}

/// Silence elapsed at `now_ms`, given the oracle's view of the user channel.
///
/// The live pipeline derives this from its VAD; replay derives it from the
/// oracle so that every detector under test sees the same clock and the
/// comparison is between detectors rather than between VAD settings.
pub fn silence_at(turns: &[SpeechSpan], now_ms: u64) -> Duration {
    let last_end = turns
        .iter()
        .filter(|s| s.start_ms <= now_ms)
        .map(|s| s.end_ms.min(now_ms))
        .max();
    match last_end {
        Some(end) => Duration::from_millis(now_ms.saturating_sub(end)),
        None => Duration::ZERO,
    }
}

/// Everything a replay needs to know about one recorded session.
pub struct Session {
    /// Channel 0, normalised to `-1.0..=1.0`.
    pub user: Vec<f32>,
    /// Channel 1, same length as `user`.
    pub agent: Vec<f32>,
    pub sample_rate: u32,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sample counts rather than the samples themselves: a session is
        // minutes of audio.
        f.debug_struct("Session")
            .field("user_samples", &self.user.len())
            .field("agent_samples", &self.agent.len())
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

impl Session {
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.user.len() as u64 * 1000 / u64::from(self.sample_rate)
    }
}

/// Split an interleaved stereo frame into the two channels.
///
/// `session_audio.wav` is written user-first; a mono file is treated as the
/// user channel with a silent agent, so a recording made before the second
/// channel existed still scores.
pub fn split_stereo(samples: &[i16], channels: u16, sample_rate: u32) -> Session {
    let to_f32 = |s: i16| f32::from(s) / f32::from(i16::MAX);
    if channels <= 1 {
        let user: Vec<f32> = samples.iter().copied().map(to_f32).collect();
        let agent = vec![0.0; user.len()];
        return Session {
            user,
            agent,
            sample_rate,
        };
    }
    let n = usize::from(channels);
    let user = samples.iter().step_by(n).copied().map(to_f32).collect();
    let agent = samples
        .iter()
        .skip(1)
        .step_by(n)
        .copied()
        .map(to_f32)
        .collect();
    Session {
        user,
        agent,
        sample_rate,
    }
}

/// Feed a detector one session and record where it committed.
///
/// Audio is handed over in `chunk_ms` steps and the detector is asked after
/// each one, which is the shape the live pipeline gives it. Detectors that
/// rate-limit themselves internally still do so here.
pub async fn replay_session(
    detector: &dyn TurnDetector,
    session: &Session,
    turns: &[SpeechSpan],
    chunk_ms: u64,
) -> Vec<Commit> {
    let mut commits = Vec::new();
    if session.sample_rate == 0 || chunk_ms == 0 {
        return commits;
    }
    let step = (u64::from(session.sample_rate) * chunk_ms / 1000).max(1) as usize;
    let transcript = replay_transcript();
    let mut committed_for_turn = false;
    let mut turn_idx = 0usize;

    for (i, start) in (0..session.user.len()).step_by(step).enumerate() {
        let end = (start + step).min(session.user.len());
        let now_ms = (i as u64 + 1) * chunk_ms;

        // Only the pipeline's own gating decides what the detector hears; the
        // replay hands over both channels exactly as the runner tees them.
        let user_i16: Vec<i16> = session.user[start..end]
            .iter()
            .map(|s| (s * f32::from(i16::MAX)) as i16)
            .collect();
        let agent_i16: Vec<i16> = session.agent[start..end]
            .iter()
            .map(|s| (s * f32::from(i16::MAX)) as i16)
            .collect();
        detector.feed_audio(&user_i16, session.sample_rate);
        detector.feed_agent_audio(&agent_i16, session.sample_rate);

        // One commit per turn: a detector that keeps saying "complete" after
        // it has already committed is not making a new decision, and counting
        // the repeats would flatter whichever one is noisiest.
        while turn_idx + 1 < turns.len() && now_ms >= turns[turn_idx + 1].start_ms {
            turn_idx += 1;
            committed_for_turn = false;
        }
        if committed_for_turn {
            continue;
        }

        let signal = detector
            .evaluate(&transcript, silence_at(turns, now_ms))
            .await;
        if signal.is_complete() {
            commits.push(Commit {
                at_ms: now_ms,
                forced: signal.decision == voice_pipes::stt::turn::TurnDecision::ForcedComplete,
            });
            committed_for_turn = true;
        }
    }
    commits
}
