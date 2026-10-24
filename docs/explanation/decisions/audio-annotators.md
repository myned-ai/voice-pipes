# Off-path audio annotators

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-08

## Context

A lip-sync service was attached through the inline post-audio chain, which
is awaited per chunk before the chunk is forwarded. The realtime engine calls
it once per ~41 ms delta. Measured: audio at 1.9× real time without it,
0.6× with it, and with the service stopped every chunk waited the full
timeout. The processor itself was fast on 160–500 ms batches; it was slow
only because it was called 25 times a second on one-second windows. A dead
metadata service must never throttle speech.

## Decision

A second processor class, `mode = "annotate"`, that runs **off the audio
path**: the hot path does one non-blocking channel send; a per-session hub
coalesces audio into `batch_ms` segments tagged with utterance id and
offset, calls the annotator with bounded concurrency over a streaming gRPC
`Annotate` RPC (unary `Process` still works, vp batches for it), and
forwards results as a new ordered `audio_annotation` frame. Timeouts drop a
batch, a circuit breaker skips a failing service for a cooldown, barge-in
cancels pending work, a warm-up segment removes the cold-start hole. The
inline chain is untouched for processors that mutate samples.

Clients place annotations by utterance offset with the existing playback
clock; payloads are opaque JSON with a free-form `kind`, so vp knows nothing
about blendshapes.

## Consequences

- Audio streams at the provider's native rate regardless of the annotator;
  lip-sync CPU dropped by roughly ten times on the measured setup.
- Annotations can arrive late; the client reports lateness in
  `playback_quality` and the metrics show batches, lag and breaker state.
- Additive config and wire changes only.

## Where in code

`voice-pipes/src/pipeline/annotate.rs` (hub), `src/pipeline/post_audio.rs`,
`proto/audio_post_processor.proto`, the built-in `level` annotator.
Guide: [Audio annotators](../../guides/audio-annotators.md).
