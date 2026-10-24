use super::*;

use std::sync::Mutex;

use async_trait::async_trait;
use voice_pipes::stt::turn::{SilenceTimeoutDetector, TurnDecision, TurnSignal};

// ── Synthetic audio ─────────────────────────────────────────

const SR: u32 = 16_000;

/// Speech-shaped audio: a tone under an amplitude envelope. Not speech, but
/// it has the property the oracle keys on — energy well above the floor —
/// without needing a recording to test against.
fn speech(ms: u64) -> Vec<f32> {
    let n = (SR as u64 * ms / 1000) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR as f32;
            let env = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
            (t * 200.0 * 2.0 * std::f32::consts::PI).sin() * env * 0.3
        })
        .collect()
}

/// Quiet, but not digitally silent: a real recording always has a floor, and
/// an oracle that only works on perfect zeros would not survive one.
fn room_tone(ms: u64) -> Vec<f32> {
    let n = (SR as u64 * ms / 1000) as usize;
    (0..n)
        .map(|i| ((i * 7919) % 101) as f32 / 101.0 * 0.002 - 0.001)
        .collect()
}

/// Build a channel from an alternating script of (`is_speech`, ms).
fn channel(script: &[(bool, u64)]) -> Vec<f32> {
    let mut out = Vec::new();
    for &(is_speech, ms) in script {
        out.extend(if is_speech { speech(ms) } else { room_tone(ms) });
    }
    out
}

// ── The oracle ──────────────────────────────────────────────

#[test]
fn finds_speech_against_room_tone() {
    let audio = channel(&[(false, 500), (true, 1000), (false, 500)]);
    let spans = speech_spans(&audio, SR, OracleConfig::default());
    assert_eq!(spans.len(), 1, "expected one span, got {spans:?}");
    // Frame quantisation means the edges land within a frame or two.
    assert!(
        (400..=600).contains(&spans[0].start_ms),
        "start {:?}",
        spans[0].start_ms
    );
    assert!(
        (1400..=1600).contains(&spans[0].end_ms),
        "end {:?}",
        spans[0].end_ms
    );
}

#[test]
fn a_short_pause_does_not_split_a_turn() {
    // The whole reason the oracle is offline: a 300 ms breath inside a
    // sentence must not read as the end of the turn.
    let audio = channel(&[(true, 800), (false, 300), (true, 800)]);
    let spans = speech_spans(&audio, SR, OracleConfig::default());
    assert_eq!(
        spans.len(),
        1,
        "a mid-sentence pause split the turn: {spans:?}"
    );
}

#[test]
fn a_long_gap_does_split_two_turns() {
    let audio = channel(&[(true, 800), (false, 1200), (true, 800)]);
    let spans = speech_spans(&audio, SR, OracleConfig::default());
    assert_eq!(spans.len(), 2, "expected two turns, got {spans:?}");
}

#[test]
fn clicks_shorter_than_min_span_are_dropped() {
    let audio = channel(&[(false, 400), (true, 60), (false, 1000)]);
    let spans = speech_spans(&audio, SR, OracleConfig::default());
    assert!(
        spans.is_empty(),
        "a 60 ms blip was counted as a turn: {spans:?}"
    );
}

#[test]
fn silence_alone_yields_no_turns() {
    let audio = room_tone(3000);
    assert!(speech_spans(&audio, SR, OracleConfig::default()).is_empty());
}

#[test]
fn the_threshold_adapts_to_a_noisy_recording() {
    // Same speech, ten times the room tone. A fixed threshold would either
    // miss the speech in the quiet file or find speech everywhere in the
    // loud one; deriving it from the file's own floor handles both.
    let quiet = channel(&[(false, 400), (true, 800), (false, 400)]);
    let noisy: Vec<f32> = channel(&[(false, 400), (true, 800), (false, 400)])
        .iter()
        .enumerate()
        .map(|(i, s)| s + (((i * 7919) % 101) as f32 / 101.0 * 0.02 - 0.01))
        .collect();
    let a = speech_spans(&quiet, SR, OracleConfig::default());
    let b = speech_spans(&noisy, SR, OracleConfig::default());
    assert_eq!(a.len(), 1, "quiet: {a:?}");
    assert_eq!(b.len(), 1, "noisy: {b:?}");
}

#[test]
fn degenerate_input_is_not_a_panic() {
    assert!(speech_spans(&[], SR, OracleConfig::default()).is_empty());
    assert!(speech_spans(&speech(100), 0, OracleConfig::default()).is_empty());
    let cfg = OracleConfig {
        frame_ms: 0,
        ..OracleConfig::default()
    };
    assert!(speech_spans(&speech(100), SR, cfg).is_empty());
}

// ── Scoring ─────────────────────────────────────────────────

fn span(start_ms: u64, end_ms: u64) -> SpeechSpan {
    SpeechSpan { start_ms, end_ms }
}

#[test]
fn a_commit_after_the_end_is_positive_latency() {
    let turns = [span(0, 1000)];
    let commits = [Commit {
        at_ms: 1200,
        forced: false,
    }];
    let scores = score_turns(&turns, &commits);
    assert_eq!(scores[0].endpoint_latency_ms, Some(200));
    assert!(!scores[0].cut_off);
}

#[test]
fn a_commit_before_the_end_is_a_cut_off_with_negative_latency() {
    // The sign is the point: this is not a fast detector, it is a wrong one,
    // and a mean that mixes the two would hide it.
    let turns = [span(0, 1000)];
    let commits = [Commit {
        at_ms: 800,
        forced: false,
    }];
    let scores = score_turns(&turns, &commits);
    assert_eq!(scores[0].endpoint_latency_ms, Some(-200));
    assert!(scores[0].cut_off);
}

#[test]
fn no_commit_is_a_hang_not_an_enormous_latency() {
    let turns = [span(0, 1000)];
    let scores = score_turns(&turns, &[]);
    assert!(scores[0].hung);
    assert_eq!(scores[0].endpoint_latency_ms, None);
    assert_eq!(scores[0].commit_ms, None);
}

#[test]
fn a_commit_is_attributed_to_the_turn_it_follows() {
    let turns = [span(0, 1000), span(3000, 4000)];
    let commits = [
        Commit {
            at_ms: 1200,
            forced: false,
        },
        Commit {
            at_ms: 4300,
            forced: false,
        },
    ];
    let scores = score_turns(&turns, &commits);
    assert_eq!(scores[0].endpoint_latency_ms, Some(200));
    assert_eq!(scores[1].endpoint_latency_ms, Some(300));
}

#[test]
fn a_commit_before_the_first_turn_is_not_attributed_backwards() {
    let turns = [span(2000, 3000)];
    let commits = [Commit {
        at_ms: 500,
        forced: false,
    }];
    let scores = score_turns(&turns, &commits);
    assert!(
        scores[0].hung,
        "a stray early commit was credited to a later turn"
    );
}

#[test]
fn a_late_commit_does_not_steal_the_next_turn() {
    // A detector that fires after the next turn has already begun has not
    // scored that turn; it has hung on this one.
    let turns = [span(0, 1000), span(2000, 3000)];
    let commits = [Commit {
        at_ms: 2500,
        forced: false,
    }];
    let scores = score_turns(&turns, &commits);
    assert!(scores[0].hung);
    assert_eq!(scores[1].endpoint_latency_ms, Some(-500));
}

// ── Aggregation ─────────────────────────────────────────────

fn late(latency: i64) -> TurnScore {
    TurnScore {
        turn_end_ms: 1000,
        commit_ms: Some((1000 + latency) as u64),
        endpoint_latency_ms: Some(latency),
        cut_off: latency < 0,
        forced: false,
        hung: false,
    }
}

#[test]
fn cut_offs_are_excluded_from_the_latency_percentiles() {
    // Two fast-but-wrong commits would drag a naive median down and make a
    // detector that cuts people off look like the quickest one.
    let scores = vec![late(-500), late(-400), late(100), late(200), late(300)];
    let agg = aggregate(&scores);
    assert_eq!(agg.median_endpoint_latency_ms, Some(200));
    assert!((agg.cut_off_rate - 0.4).abs() < 1e-9);
}

#[test]
fn percentiles_and_rates_over_a_mixed_run() {
    let mut scores: Vec<TurnScore> = (1..=10).map(|i| late(i * 100)).collect();
    scores.push(TurnScore {
        turn_end_ms: 1000,
        commit_ms: None,
        endpoint_latency_ms: None,
        cut_off: false,
        forced: false,
        hung: true,
    });
    let agg = aggregate(&scores);
    assert_eq!(agg.turns, 11);
    // Ten latencies, 100..1000. Nearest-rank has no single middle for an even
    // count and takes the upper one, which for a latency metric is the right
    // way to be wrong: it reports the slower of the two rather than the
    // flattering one.
    assert_eq!(agg.median_endpoint_latency_ms, Some(600));
    assert_eq!(agg.p90_endpoint_latency_ms, Some(900));
    assert!((agg.hang_rate - 1.0 / 11.0).abs() < 1e-9);
}

#[test]
fn an_empty_run_aggregates_to_zero_rather_than_dividing_by_zero() {
    let agg = aggregate(&[]);
    assert_eq!(agg.turns, 0);
    assert_eq!(agg.median_endpoint_latency_ms, None);
    assert!((agg.cut_off_rate - 0.0).abs() < 1e-9);
}

#[test]
fn forced_commits_are_counted_separately() {
    let scores = vec![
        TurnScore {
            forced: true,
            ..late(900)
        },
        late(200),
    ];
    let agg = aggregate(&scores);
    assert!((agg.forced_rate - 0.5).abs() < 1e-9);
}

// ── Replay plumbing ─────────────────────────────────────────

#[test]
fn stereo_splits_user_first() {
    // user, agent, user, agent, ...
    let interleaved = [100i16, -100, 200, -200, 300, -300];
    let s = split_stereo(&interleaved, 2, SR);
    assert_eq!(s.user.len(), 3);
    assert!(s.user[0] > 0.0 && s.agent[0] < 0.0, "channels are swapped");
}

#[test]
fn a_mono_recording_is_treated_as_the_user_with_a_silent_agent() {
    let s = split_stereo(&[100i16, 200, 300], 1, SR);
    assert_eq!(s.user.len(), 3);
    assert_eq!(s.agent, vec![0.0, 0.0, 0.0]);
}

#[test]
fn silence_is_measured_from_the_end_of_the_last_turn() {
    let turns = [span(0, 1000), span(3000, 4000)];
    assert_eq!(silence_at(&turns, 1500), Duration::from_millis(500));
    // Inside a turn there is no silence.
    assert_eq!(silence_at(&turns, 3500), Duration::ZERO);
    // Before any speech, nothing has ended yet.
    assert_eq!(silence_at(&[span(2000, 3000)], 1000), Duration::ZERO);
}

#[test]
fn the_replay_transcript_is_non_empty() {
    // At least one detector skips inference on an empty transcript, so a
    // blank placeholder would silently score it as never firing.
    assert!(!replay_transcript().text.trim().is_empty());
}

/// Commits after a fixed wall-clock instant, to check replay bookkeeping
/// without depending on any real detector's behaviour.
#[expect(
    clippy::struct_field_names,
    reason = "every field is a duration in milliseconds, and the suffix names the unit"
)]
struct FiresAt {
    after_ms: u64,
    elapsed_ms: Mutex<u64>,
    chunk_ms: u64,
}

#[async_trait]
impl TurnDetector for FiresAt {
    async fn evaluate(&self, _r: &TranscriptionResult, _silence: Duration) -> TurnSignal {
        let mut e = self.elapsed_ms.lock().expect("test mutex");
        *e += self.chunk_ms;
        if *e >= self.after_ms {
            TurnSignal::likely_complete()
        } else {
            TurnSignal::incomplete()
        }
    }

    fn name(&self) -> &'static str {
        "fires-at"
    }
}

#[tokio::test]
async fn replay_records_one_commit_per_turn() {
    let session = Session {
        user: channel(&[(true, 600), (false, 1200), (true, 600)]),
        agent: room_tone(2400),
        sample_rate: SR,
    };
    let turns = speech_spans(&session.user, SR, OracleConfig::default());
    assert_eq!(
        turns.len(),
        2,
        "fixture should contain two turns: {turns:?}"
    );

    let det = FiresAt {
        after_ms: 200,
        elapsed_ms: Mutex::new(0),
        chunk_ms: 20,
    };
    let commits = replay_session(&det, &session, &turns, 20).await;

    assert_eq!(
        commits.len(),
        2,
        "a detector that keeps saying complete must still score once per turn, got {commits:?}"
    );
}

#[tokio::test]
async fn a_real_silence_detector_commits_after_its_timeout() {
    // End to end over the actual built-in detector: one turn, then a gap
    // longer than the timeout, so the commit must land in that gap.
    let session = Session {
        user: channel(&[(true, 800), (false, 2000)]),
        agent: room_tone(2800),
        sample_rate: SR,
    };
    let turns = speech_spans(&session.user, SR, OracleConfig::default());
    assert_eq!(turns.len(), 1);

    let det = SilenceTimeoutDetector::new(500);
    let commits = replay_session(&det, &session, &turns, 20).await;
    assert_eq!(commits.len(), 1, "expected one commit, got {commits:?}");
    assert!(commits[0].forced, "the silence detector commits by timeout");

    let scores = score_turns(&turns, &commits);
    let latency = scores[0]
        .endpoint_latency_ms
        .expect("a commit was recorded");
    assert!(
        (400..=700).contains(&latency),
        "a 500 ms timeout should show as roughly 500 ms of endpoint latency, got {latency}"
    );
    assert!(!scores[0].cut_off);
}

#[tokio::test]
async fn an_eager_detector_shows_up_as_a_cut_off() {
    struct AlwaysComplete;
    #[async_trait]
    impl TurnDetector for AlwaysComplete {
        async fn evaluate(&self, _r: &TranscriptionResult, _s: Duration) -> TurnSignal {
            TurnSignal::likely_complete()
        }
        fn name(&self) -> &'static str {
            "always"
        }
    }

    let session = Session {
        user: channel(&[(true, 1000), (false, 1000)]),
        agent: room_tone(2000),
        sample_rate: SR,
    };
    let turns = speech_spans(&session.user, SR, OracleConfig::default());
    let commits = replay_session(&AlwaysComplete, &session, &turns, 20).await;
    let agg = aggregate(&score_turns(&turns, &commits));
    assert!(
        (agg.cut_off_rate - 1.0).abs() < 1e-9,
        "a detector that always commits should score 100% cut-off, got {agg:?}"
    );
}

// ── Reporting ───────────────────────────────────────────────

#[test]
fn the_table_reports_every_detector_and_marks_missing_numbers() {
    let rows = vec![
        (
            "silence".to_string(),
            aggregate(&[late(500), late(600), late(700)]),
        ),
        ("never".to_string(), DetectorScore::default()),
    ];
    let table = render_table(&rows);
    assert!(table.contains("silence"));
    assert!(table.contains("never"));
    assert!(table.contains("600"), "median should appear: {table}");
    assert!(
        table.contains('-'),
        "a detector with no latencies should show a dash, not 0: {table}"
    );
}

#[test]
fn decision_variants_map_to_the_forced_flag() {
    assert_eq!(
        TurnSignal::forced_complete().decision,
        TurnDecision::ForcedComplete
    );
    assert_ne!(
        TurnSignal::likely_complete().decision,
        TurnDecision::ForcedComplete
    );
}
