//! Gated end-to-end synthetic-caller test (T7.11).
//!
//! Exercises the full [`SyntheticCaller`] surface against the `OpenAI`
//! Realtime API. Disabled by default; opt in with both:
//!
//! - `RUN_SYNTHETIC_CALLER=1`
//! - `OPENAI_API_KEY=<real key>`
//!
//! Run with:
//! ```text
//! cargo test --release --features synthetic-caller \
//!     --test synthetic_caller_openai_live -- --ignored --nocapture
//! ```
//!
//! Cost guardrails: caps the run at 2 turns and 15 seconds. The
//! `text` bridge mode is used so the test does not depend on a live
//! pipeline-side STT model.

#![cfg(feature = "synthetic-caller")]

use std::time::{Duration, Instant};

use voice_pipes_bench::synthetic_caller::openai_realtime::OpenAiRealtimeCallerBuilder;
use voice_pipes_bench::synthetic_caller::{CallerEvent, SyntheticCaller};

fn gate_satisfied() -> bool {
    std::env::var("RUN_SYNTHETIC_CALLER").ok().as_deref() == Some("1")
        && std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.is_empty())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live test: requires OPENAI_API_KEY and network"]
async fn openai_realtime_completes_a_short_turn() {
    assert!(
        gate_satisfied(),
        "set RUN_SYNTHETIC_CALLER=1 and OPENAI_API_KEY to enable this test"
    );

    // Connect to the Realtime API.
    let mut caller = OpenAiRealtimeCallerBuilder::default()
        .connect()
        .await
        .expect("connect to OpenAI Realtime API");

    // Terse persona keeps the response short and bounds cost.
    caller
        .configure(
            "You are a benchmark probe. Reply with exactly one word: 'ok'. \
             Do not say anything else.",
        )
        .await
        .expect("configure caller");

    // Pull events until the first ResponseDone or a 15-second cap.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut got_audio = false;
    let mut got_response_done = false;
    let mut transcript = String::new();

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let evt = tokio::time::timeout(remaining, caller.next_event()).await;
        match evt {
            Ok(Some(CallerEvent::AudioDelta(samples))) => {
                if !samples.is_empty() {
                    got_audio = true;
                }
            }
            Ok(Some(CallerEvent::TranscriptDelta(t))) => transcript.push_str(&t),
            Ok(Some(CallerEvent::ResponseDone)) => {
                got_response_done = true;
                break;
            }
            Ok(Some(CallerEvent::Error(e))) => panic!("caller error: {e}"),
            Ok(None) => panic!("caller stream closed before ResponseDone"),
            Err(_) => break,
        }
    }

    let _ = Box::new(caller).close().await;

    assert!(got_response_done, "no ResponseDone within budget");
    assert!(got_audio, "no audio delta received from Realtime API");
    assert!(
        !transcript.trim().is_empty(),
        "expected at least one transcript fragment"
    );
}
