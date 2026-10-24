use super::super::turn::{
    AlignedWord, AnchorSource, PendingInterruption, RealtimeTurn, RequestAnchor, truncate_aligned,
};
use crate::pipeline::TurnMetricsSnapshot;
use std::time::Instant;

#[test]
fn test_cumulative_timing_offset_and_truncation() {
    // 1. Start of first turn: session_audio_emitted_ms is 0.
    let mut session_audio_emitted_ms = 0;
    let mut turn = RealtimeTurn::new(1, session_audio_emitted_ms);
    assert_eq!(turn.estimated_text_ms, 0);
    assert_eq!(turn.start_audio_ms, 0);

    // Emit text delta "hello" at offset 0
    let audio_ms_now = turn.estimated_text_ms;
    turn.assistant_text.push_str("hello");
    turn.text_timeline
        .push((audio_ms_now, turn.assistant_text.len()));
    // Advance text offset
    turn.estimated_text_ms = turn.estimated_text_ms.saturating_add(500);

    // Downlink some audio samples (say, 48000 samples at 24kHz = 2000 ms)
    turn.audio_samples = 48000;
    turn.output_sample_rate = Some(24000);
    assert_eq!(turn.audio_emitted_ms(), 2000);
    session_audio_emitted_ms += 2000;

    // 2. Start of second turn: session_audio_emitted_ms is 2000.
    let mut turn2 = RealtimeTurn::new(2, session_audio_emitted_ms);
    assert_eq!(turn2.estimated_text_ms, 2000);
    assert_eq!(turn2.start_audio_ms, 2000);

    // Emit text delta "world" at offset 2000
    let audio_ms_now2 = turn2.estimated_text_ms;
    turn2.assistant_text.push_str("world");
    turn2
        .text_timeline
        .push((audio_ms_now2, turn2.assistant_text.len()));
    turn2.estimated_text_ms = turn2.estimated_text_ms.saturating_add(500);

    // Downlink some audio for second turn (e.g. 72000 samples at 24kHz = 3000 ms)
    turn2.audio_samples = 72000;
    turn2.output_sample_rate = Some(24000);
    assert_eq!(turn2.audio_emitted_ms(), 3000);
    let _ = session_audio_emitted_ms + 3000;

    // Verify the timelines
    assert_eq!(turn.text_timeline, vec![(0, 5)]);
    assert_eq!(turn2.text_timeline, vec![(2000, 5)]);

    // 3. Client interrupts second turn. We capture pending interruption.
    let pending = PendingInterruption {
        utterance_id: turn2.utterance_id,
        full_text: turn2.assistant_text.clone(),
        text_timeline: turn2.text_timeline.clone(),
        audio_emitted_ms: turn2.audio_emitted_ms(),
        item_id: turn2.item_id.clone(),
        start_audio_ms: turn2.start_audio_ms,
        aligned_words: Vec::new(),
        snapshot: TurnMetricsSnapshot::default(),
        user_transcript: None,
    };

    // Client reports last_played_offset_ms = 3500 (session-cumulative).
    // That means from 2000ms (start of turn) to 3500ms (played offset).
    let last_played_offset_ms = 3500_u32;

    let turn_played_ms = (last_played_offset_ms as u64)
        .saturating_sub(pending.start_audio_ms)
        .min(pending.audio_emitted_ms);
    let session_played_ms = pending.start_audio_ms.saturating_add(turn_played_ms);

    assert_eq!(turn_played_ms, 1500); // 3500 - 2000
    assert_eq!(session_played_ms, 3500);

    // Verify text truncation using session_played_ms
    let view = RealtimeTurn {
        utterance_id: pending.utterance_id,
        user_transcript: None,
        started_at: Instant::now(),
        ttfb_anchor: None,
        ttfb_ms: None,
        first_text_ms: None,
        speaking_announced: false,
        vp_speech_end: None,
        audio_samples: 0,
        output_sample_rate: None,
        assistant_text: pending.full_text.clone(),
        text_delta_idx: 0,
        item_id: None,
        text_timeline: pending.text_timeline.clone(),
        output_fragment_idx: 0,
        estimated_text_ms: pending.audio_emitted_ms,
        start_audio_ms: pending.start_audio_ms,
        saw_function_call: false,
        opened_by_tool_call: false,
        withheld_transcript: Vec::new(),
        aligned_words: Vec::new(),
        output: crate::pipeline::OutputThroughput::default(),
        chain: crate::pipeline::ChainStats::default(),
    };

    let truncated = view.truncate_text_at(session_played_ms);
    assert_eq!(truncated, "world");
}

/// A finished realtime turn carries the
/// `output` component (emit/source rtf, chain overhead) and one
/// `audio_post:<name>` per inline processor next to the fused `realtime`
/// component, so a slow post-chain is attributable instead of being folded
/// into the model's `total_ms`.
#[test]
fn realtime_turn_snapshot_carries_output_and_post_chain_components() {
    let mut turn = RealtimeTurn::new(7, 0);
    turn.output_sample_rate = Some(16_000);
    turn.audio_samples = 3200; // 200 ms
    turn.output.on_source(100);
    turn.output.on_emit();
    turn.output.on_source(100);
    turn.output.on_emit();
    turn.chain
        .record("grpc", 5, 100, crate::pipeline::CallOutcome::Ok);
    turn.chain
        .record("grpc", 2000, 100, crate::pipeline::CallOutcome::Timeout);

    let snapshot = turn.finish(false, 0, "gemini-live");
    let ids: Vec<&str> = snapshot
        .components
        .iter()
        .map(|c| c.component_id.as_str())
        .collect();
    assert_eq!(ids, vec!["realtime", "output", "audio_post:grpc"]);
    let output = snapshot.component("output").unwrap();
    assert_eq!(output.kind, "output");
    assert_eq!(output.extras["audio_output_ms"], 200);
    assert!(output.extras["emit_rtf"].is_number());
    assert!(output.extras["source_rtf"].is_number());
    assert!(output.extras["chain_overhead_ms"].is_number());
    let post = snapshot.component("audio_post:grpc").unwrap();
    assert_eq!(post.kind, "audio_post");
    assert_eq!(post.extras["calls"], 2);
    assert_eq!(post.extras["timeouts"], 1);
    assert_eq!(post.extras["budget_ratio"], 10.03); // (2005/2) / 100

    // No audio → no output component, no chain calls → no post component.
    let empty = RealtimeTurn::new(8, 0).finish(true, 0, "gemini-live");
    assert_eq!(empty.components.len(), 1);
}

// ── word-exact truncation from provider alignment ───────────

fn aligned(words: &[(&str, u64)]) -> Vec<AlignedWord> {
    words
        .iter()
        .map(|(w, end)| AlignedWord {
            word: (*w).to_string(),
            end_ms: *end,
        })
        .collect()
}

#[test]
fn truncate_aligned_is_word_exact() {
    let text = "Hello there. How can I help you today?";
    let words = aligned(&[
        ("Hello", 244),
        ("there.", 534),
        ("How", 720),
        ("can", 895),
        ("I", 1531),
        ("help", 1650),
        ("you", 1800),
        ("today?", 2300),
    ]);
    // Played through "there." but barely into "How".
    assert_eq!(
        truncate_aligned(&words, text, 600).as_deref(),
        Some("Hello there.")
    );
    // Played everything.
    assert_eq!(truncate_aligned(&words, text, 9_999).as_deref(), Some(text));
    // Played nothing.
    assert_eq!(truncate_aligned(&words, text, 0).as_deref(), Some(""));
}

#[test]
fn truncate_aligned_handles_split_words_and_odd_spacing() {
    // A word split across two audio chunks arrives as two partials;
    // double spaces in the transcript are skipped by the cursor.
    let text = "Hel lo  world";
    let words = aligned(&[("Hel", 100), ("lo", 200), ("world", 500)]);
    assert_eq!(
        truncate_aligned(&words, text, 250).as_deref(),
        Some("Hel lo")
    );
}

#[test]
fn truncate_aligned_bails_on_transcript_divergence() {
    // Normalization drift (alignment says "two", transcript says "2"):
    // the aligned path must refuse so callers use the estimate.
    let words = aligned(&[("one", 100), ("two", 200)]);
    assert_eq!(truncate_aligned(&words, "one 2 three", 150), None);
}

#[test]
fn aligned_words_take_precedence_over_coarse_timeline() {
    // ElevenLabs delivers its transcript as ONE text delta, so the
    // estimate timeline has a single entry covering the whole text —
    // all-or-nothing. With alignment present, truncation is per-word.
    let mut turn = RealtimeTurn::new(1, 0);
    turn.assistant_text = "Alpha beta gamma".to_string();
    turn.text_timeline = vec![(0, turn.assistant_text.len())];
    turn.aligned_words = aligned(&[("Alpha", 400), ("beta", 800), ("gamma", 1200)]);
    // Word-exact: at 500 ms only "Alpha" has finished playing.
    assert_eq!(turn.truncate_text_at(500), "Alpha");
    // The estimate path can only see the single coarse entry: the same
    // report over-retains everything up to the whitespace snap.
    turn.aligned_words.clear();
    assert_eq!(turn.truncate_text_at(500), "Alpha beta");
}

/// The end-of-user-speech wall clock must reach the snapshot extras only
/// when a genuine anchor was captured — an unanchored turn measures TTFB
/// from its own creation, which is meaningless as an E2E anchor and must
/// not be surfaced (the admin reader keys its perceived-E2E fallback on
/// this extra's presence).
#[test]
fn finish_stamps_user_speech_end_epoch_ms_only_when_anchored() {
    let mut turn = RealtimeTurn::new(1, 0);
    turn.ttfb_anchor = Some(RequestAnchor::now(AnchorSource::ProviderSpeechStopped));
    let snap = turn.finish(false, 0, "test_provider");
    let epoch = snap.components[0]
        .extras
        .get("user_speech_end_epoch_ms")
        .and_then(serde_json::Value::as_u64)
        .expect("anchored turn must expose user_speech_end_epoch_ms");
    // Sanity: a plausible epoch-ms value (after 2020-01-01).
    assert!(epoch > 1_577_836_800_000);

    let turn = RealtimeTurn::new(2, 0);
    let snap = turn.finish(false, 0, "test_provider");
    assert!(
        !snap.components[0]
            .extras
            .contains_key("user_speech_end_epoch_ms"),
        "unanchored turn must not expose a speech-end wall clock"
    );
}

/// A withdrawn call's id answers once, and the set stays bounded: the
/// oldest ids go first.
#[test]
fn withdrawn_calls_answer_once_and_stay_bounded() {
    use super::super::turn::WithdrawnCalls;

    let mut withdrawn = WithdrawnCalls::default();
    withdrawn.withdraw(["a".to_string(), "b".to_string()]);
    assert!(withdrawn.take("b"));
    assert!(!withdrawn.take("b"), "an id answers once");
    assert!(!withdrawn.take("c"), "a call never withdrawn");

    withdrawn.withdraw((0..WithdrawnCalls::CAPACITY).map(|i| format!("x{i}")));
    assert!(!withdrawn.take("a"), "the oldest id made room");
    assert!(withdrawn.take("x0"));
    assert!(withdrawn.take(&format!("x{}", WithdrawnCalls::CAPACITY - 1)));

    withdrawn.withdraw(["d".to_string()]);
    withdrawn.reset();
    assert!(
        !withdrawn.take("d"),
        "a reconnect forgets the old session's calls"
    );
}

/// How a close reads against the session's resumption state: a refusal by
/// the protocol's words or by an unacknowledged resumed connection spends
/// the token; a fatal close stays fatal.
#[test]
fn resumption_reads_closes() {
    use super::super::turn::{CloseReading, Resumption};
    use crate::realtime::protocol::CloseClass;
    use voice_pipes_sdk::secrets::Secret;

    let kept = |class| CloseReading {
        class,
        refused_resumption: false,
    };
    let refused = CloseReading {
        class: CloseClass::Transient,
        refused_resumption: true,
    };

    // No token: closes read as the protocol classed them, and a refusal
    // with nothing to refuse is final.
    let mut r = Resumption::default();
    assert!(!r.can_resume());
    assert_eq!(
        r.on_close(CloseClass::Transient),
        kept(CloseClass::Transient)
    );
    assert_eq!(r.on_close(CloseClass::Unresumable), kept(CloseClass::Fatal));

    // A token, and a connection that drops after its session was
    // acknowledged: resume from the same token.
    r.issued(Secret::new("t1"));
    assert_eq!(r.for_reconnect(), Some(Secret::new("t1")));
    r.confirmed();
    assert_eq!(
        r.on_close(CloseClass::Transient),
        kept(CloseClass::Transient)
    );
    assert!(r.can_resume());

    // A resumed connection that closes before its acknowledgement, in
    // whatever words: refused, and the next connection is fresh.
    assert!(r.for_reconnect().is_some());
    assert_eq!(r.on_close(CloseClass::Transient), refused);
    assert!(!r.can_resume());
    assert_eq!(r.for_reconnect(), None);

    // The protocol's own reading of a refusal.
    r.issued(Secret::new("t2"));
    assert_eq!(r.on_close(CloseClass::Unresumable), refused);

    // A fatal close stays fatal, token or not.
    r.issued(Secret::new("t3"));
    let _ = r.for_reconnect();
    assert_eq!(r.on_close(CloseClass::Fatal), kept(CloseClass::Fatal));
}
