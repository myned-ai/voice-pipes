# Honest speech-to-speech latency

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-08

## Context

A Gemini Live turn reported a 1.6 s time to first byte; replaying the same
utterance with a scripted caller and stamping every hop against the true
end of speech showed the user waited 3.5 s. Three measurement defects:
the timer was stamped on the first output *transcript* delta while the
transcript was withheld until the first audio; the end-of-speech anchor was
the last input transcript delta, 0.45–1.0 s after the user actually stopped;
and a slow speech-to-speech turn could never raise an anomaly because the
rule keyed on a field that is null for realtime turns.

## Decision

- `ttfb_ms` is stamped on the first **audio** delta, and the snapshot
  carries `ttfb_anchor_source` naming what the timer started from: the
  provider's speech-stopped event, vp's own endpoint observer, or the
  client's `speech_end`, in that precedence.
- A vp-side **endpoint observer** on the uplink (energy or Silero,
  measurement only, never gating audio) gives every speech-to-speech turn
  `provider_endpointing_ms`: how late the provider noticed the user stopped.
- Clients send `speech_end` from their own VAD promptly, once per turn, and
  the backend's anchor precedence uses it as a fallback.
- The scripted probe that found the defects is the regression tool.

Verified and therefore not chased: the vp audio path adds nothing; uplink
batching is not a lever; the opening 640–960 ms blob is the provider's own
buffering.

## Consequences

- Reported numbers are anchored where they claim to be, and the anchor is
  on the wire.
- The provider's endpointing cost is a first-class metric instead of being
  folded into generation time.
- Speech-to-speech turns can now raise `tts.ttfb_high`.

## Where in code

`voice-pipes/src/transport/serve/realtime_runner/` (anchoring and
`EndpointObserver`), `src/config/types.rs` (`EndpointObserverConfig`),
`voice-pipes-admin/backend/app/captures/reader.py` (anchor precedence),
`scripts/s2s_probe.py`.
