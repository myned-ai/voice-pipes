//! Compare turn detectors on recorded conversations.
//!
//! Each framework ships a turn detector; none ship a way to find out whether
//! it works on your calls. Point this at a directory of sessions recorded with
//! `[pipeline.audit] record_session_audio` and it prints a table: for every
//! detector, how long past the real end of a turn it waited, how often it cut
//! the caller off, and how often it fell through to the silence timeout.
//!
//! ```sh
//! VP_TURN_SESSIONS=voice-pipes/captures \
//!   cargo test --release -p voice-pipes-bench --test turn_detection -- --ignored --nocapture
//! ```
//!
//! With a VAP export available, add it to the comparison:
//!
//! ```sh
//! VP_TURN_SESSIONS=voice-pipes/captures \
//! VP_VAP_MODEL=models/vap/vap-2s.onnx \
//!   cargo test --release -p voice-pipes-bench --test turn_detection \
//!     --features turn-vap -- --ignored --nocapture
//! ```
//!
//! The ground truth is derived, not labelled: an offline voice-activity pass
//! over the user's channel, which may use the whole recording and so can place
//! a turn boundary more precisely than any live detector. See
//! `voice_pipes_bench::turn_detection` for what the numbers mean.
//!
//! `provider` mode is deliberately absent. Its entire input is the
//! `is_end_of_turn` flag a real recogniser sets, and replay has no recogniser,
//! so scoring it here would measure nothing.

use std::path::{Path, PathBuf};

use voice_pipes::audio::wav_reader::read_wav;
use voice_pipes::stt::turn::{SilenceTimeoutDetector, TurnDetector};
use voice_pipes_bench::turn_detection::{
    DetectorScore, OracleConfig, aggregate, render_table, replay_session, score_turns,
    speech_spans, split_stereo,
};

/// How often the detector is asked, in milliseconds of audio.
const CHUNK_MS: u64 = 20;

/// A named detector and a way to build a fresh one.
///
/// Boxed constructor rather than an instance: each detector is rebuilt per
/// session so none carries a rolling audio buffer across recordings.
type Candidate = (String, Box<dyn Fn() -> Box<dyn TurnDetector>>);

/// Collect `session_audio.wav` under a captures directory.
fn find_sessions(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join("session_audio.wav");
        if candidate.is_file() {
            out.push(candidate);
        }
    }
    out.sort();
    out
}

/// The detectors to compare.
fn detectors() -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for timeout in [500u64, 700, 1000] {
        out.push((
            format!("silence@{timeout}ms"),
            Box::new(move || Box::new(SilenceTimeoutDetector::new(timeout))),
        ));
    }
    #[cfg(feature = "turn-vap")]
    if let Ok(model) = std::env::var("VP_VAP_MODEL") {
        for threshold in [0.4f32, 0.5, 0.6] {
            let model = model.clone();
            out.push((
                format!("vap@{threshold}"),
                Box::new(move || {
                    Box::new(
                        voice_pipes_turn_vap::VapTurnDetector::new(
                            Path::new(&model),
                            threshold,
                            200,
                            2000,
                        )
                        .expect("VP_VAP_MODEL loads"),
                    )
                }),
            ));
        }
    }
    out
}

#[tokio::test]
#[ignore = "needs sessions recorded with record_session_audio; set VP_TURN_SESSIONS"]
async fn compare_turn_detectors_on_recorded_sessions() {
    let Ok(root) = std::env::var("VP_TURN_SESSIONS") else {
        panic!("set VP_TURN_SESSIONS to a captures directory")
    };
    let sessions = find_sessions(Path::new(&root));
    assert!(
        !sessions.is_empty(),
        "no session_audio.wav under {root} -- record some with \
         [pipeline.audit] record_session_audio = true"
    );

    let oracle = OracleConfig::default();
    let candidates = detectors();
    let mut rows: Vec<(String, DetectorScore)> = Vec::new();
    let mut total_turns = 0usize;

    for (name, build) in &candidates {
        let mut all_scores = Vec::new();
        for path in &sessions {
            let frame = read_wav(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let session = split_stereo(&frame.samples, frame.channels, frame.sample_rate);
            let turns = speech_spans(&session.user, session.sample_rate, oracle);
            if turns.is_empty() {
                continue;
            }
            let detector = build();
            let commits = replay_session(detector.as_ref(), &session, &turns, CHUNK_MS).await;
            all_scores.extend(score_turns(&turns, &commits));
        }
        total_turns = total_turns.max(all_scores.len());
        rows.push((name.clone(), aggregate(&all_scores)));
    }

    println!(
        "\n{} session(s), {} user turn(s) found by the offline oracle\n",
        sessions.len(),
        total_turns
    );
    println!("{}", render_table(&rows));

    assert!(
        total_turns > 0,
        "the oracle found no turns in any session -- if the recordings do \
         contain speech, the oracle's thresholds are wrong for this audio \
         and the numbers above would be meaningless"
    );
}
