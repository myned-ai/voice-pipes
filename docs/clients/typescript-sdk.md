# TypeScript SDK

**Status:** draft
**Last verified:** 2026-09-06
**Owner:** Docs

A framework-agnostic TypeScript client exists and is what the admin console's
Playground runs on. It is not yet published as its own package; extracting it
to `@voice-pipes/client` is on the [roadmap](../roadmap.md). Until then, use
it by copying the source, as described in
[React SDK integration](react-sdk-integration.md).

## What it contains

Under `voice-pipes-admin/frontend/src/lib/`:

| Module | Role |
|---|---|
| `sdk/PureVoicePipesClientFsm.ts` | The pure turn-level state machine: no I/O, deterministic transitions from events to action directives, fully unit-tested |
| `sdk/VoicePipesSessionCore.ts` | The session controller: connects the WebSocket adapter, clock sync, playback clock and the FSM; emits `stateChange`, audio, text, word-highlight, annotation and error events |
| `sdk/react/useVoicePipesSession.ts` | A React hook over the session core |
| `transport/` | WebSocket adapter, binary frame codec, NTP-lite clock sync |
| `audio/` | Microphone capture, jitter-buffered playback with a 200 ms pre-buffer, caption sync and word-timing tables |

The design and the transition table are specified in
[Building a client §12](building-a-client.md#12-pure-turn-level-client-state-machine-specification).

## Behaviour you get for free

- Handshake and session resume with token rotation.
- Sample-rate negotiation applied to capture and playback.
- The `audio_meta` FIFO pairing that keeps captions in step with audio.
- Karaoke word highlighting driven by server word timings and the local
  playback clock, with an end-of-utterance flush so sentence tails always
  complete.
- Barge-in: local flush on `clear_audio`, `playback_report` with the exact
  offset heard, `playback_complete`, transcript truncation.
- Playback-quality reporting (underrun gaps) once per turn.
- Clean-versus-abnormal disconnect detection.
- Client-side tool dispatch: `trigger_action` in, `client_tool_result` out.
- `audioAnnotation` events for lip-sync and other annotators, placed by
  utterance offset.
- Runtime-loaded extensions with a readiness gate; see
  [Playground extensions](playground-extensions.md).

## Tests

`cd voice-pipes-admin/frontend && npx vitest run src/lib/sdk/__tests__`
runs the FSM and session-core suites; CI runs the same command.
