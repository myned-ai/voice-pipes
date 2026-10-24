/// The trace-level frame log must never carry a credential.
#[test]
fn redact_secrets_masks_credential_keys_at_any_depth() {
    let frame = serde_json::json!({
        "type": "session.created",
        "session": {
            "id": "sess_1",
            "client_secret": { "value": "ek_live_abc", "expires_at": 1 },
            "instructions": "Be brief.",
            "keyterms": ["voice", "pipes"]
        },
        "headers": { "xi-api-key": "xi-123", "Authorization": "Bearer t" },
        "key": "AIza-gemini",
        "token": "tok",
        "nested": [ { "apiKey": "x" }, "plain" ]
    });
    let red = super::redact_secrets(&frame);
    let s = red.to_string();
    for leaked in [
        "ek_live_abc",
        "xi-123",
        "Bearer t",
        "AIza-gemini",
        "\"tok\"",
        "\"x\"",
    ] {
        assert!(!s.contains(leaked), "leaked {leaked}: {s}");
    }
    assert_eq!(red["session"]["instructions"], "Be brief.");
    assert_eq!(red["session"]["keyterms"][0], "voice");
    assert_eq!(red["session"]["client_secret"], "***");
    assert_eq!(red["nested"][1], "plain");
}

use super::{UPLINK_FRAME_MS, UplinkCoalescer};

#[test]
fn coalescer_buffers_tiny_chunks_until_threshold() {
    // 128-sample AudioWorklet quanta at 48 kHz (~2.67 ms each);
    // 20 ms = 960 samples, so the 8th quantum crosses the line.
    let mut c = UplinkCoalescer::new(UPLINK_FRAME_MS);
    let quantum = vec![1i16; 128];
    for _ in 0..7 {
        assert!(c.push(&quantum, 48_000).is_empty());
    }
    let frames = c.push(&quantum, 48_000);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0.len(), 128 * 8);
    assert_eq!(frames[0].1, 48_000);
}

#[test]
fn coalescer_passes_large_chunks_through() {
    // A 100 ms chunk (bench callers / CLI) flushes on the same call.
    let mut c = UplinkCoalescer::new(UPLINK_FRAME_MS);
    let chunk = vec![1i16; 4_800];
    let frames = c.push(&chunk, 48_000);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0.len(), 4_800);
}

#[test]
fn coalescer_flushes_old_rate_on_rate_change() {
    let mut c = UplinkCoalescer::new(UPLINK_FRAME_MS);
    assert!(c.push(&[1i16; 128], 48_000).is_empty());
    let frames = c.push(&[2i16; 1_000], 16_000);
    assert_eq!(frames.len(), 2);
    assert_eq!((frames[0].0.len(), frames[0].1), (128, 48_000));
    assert_eq!((frames[1].0.len(), frames[1].1), (1_000, 16_000));
}

/// Every provider's tool call passes one rule where it enters the engine:
/// no call id or no name is a recoverable error, never dispatched (the
/// result could not be routed back); no arguments is `{}`. The three
/// providers used to disagree: `OpenAI` dropped an id-less call silently
/// and sent "" as the arguments, Gemini and `ElevenLabs` dispatched it with
/// an empty id.
#[test]
fn an_incomplete_tool_call_follows_one_rule() {
    use super::RealtimeEvent;
    let call = |id: &str, name: &str, args: &str| {
        super::bounded(
            RealtimeEvent::FunctionCall {
                call_id: id.into(),
                name: name.into(),
                arguments: args.into(),
            },
            "test",
        )
    };
    for (id, name) in [("", "lookup"), ("c1", ""), (" ", "lookup")] {
        assert!(
            matches!(
                call(id, name, "{}"),
                RealtimeEvent::Error { recoverable: true, ref code, .. } if code.as_deref() == Some("tool_call_incomplete")
            ),
            "{id:?} {name:?}"
        );
    }
    for args in ["", "  "] {
        assert!(
            matches!(call("c1", "lookup", args), RealtimeEvent::FunctionCall { ref arguments, .. } if arguments == "{}"),
            "{args:?}"
        );
    }
    assert!(matches!(
        call("c1", "lookup", r#"{"q":1}"#),
        RealtimeEvent::FunctionCall { ref arguments, .. } if arguments == r#"{"q":1}"#
    ));
}
