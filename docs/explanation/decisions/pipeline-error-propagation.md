# Pipeline error propagation and session modes

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-05

## Context

Errors inside the pipeline were logged and lost; the client saw a silent
turn. Text input and structured content (tool calls, cards, images) had
separate ad hoc message types, and every session assumed voice in and voice
out.

## Decision

- One `PipelineMessage` envelope (`kind`, `text`, `payload`, `binary`,
  `role`, `turn_id`, `message_id`) replaces the separate text and data
  messages in both directions. `kind` is an open string; clients ignore
  kinds they do not know.
- `ServerControl::Error` carries a numeric `code`, a `severity`
  (`warning`, `error`, `fatal`), the originating `component` and the
  `turn_id`. Eleven pipeline error sites feed it. Deduplication uses the
  cancellation tokens already in place for barge-in.
- Five session modes negotiated at `configure`: `voice`, `text`, `hybrid`,
  `voice_in_text_out`, `text_in_voice_out`. STT and barge-in are gated by
  the mode; audio arriving in a text-only session is dropped with a warning.
- LLM output gets a `sideband` slot so structured content can travel next to
  the spoken text. (Removed 2026-09: no processor ever set it. Structured
  content reaches the client as an agent `Message` event instead.)

## Consequences

- Clients route errors through one handler keyed on the numeric code and
  bucket unknown codes by range.
- The legacy text and data message variants were removed in one cut rather
  than kept through a transition window; no production client depended on
  them.
- Rendering of non-text kinds in the Playground and tool-call parsing in
  built-in LLM providers were follow-ups (client-side tools shipped later).

## Where in code

`voice-pipes/src/transport/mod.rs` (`PipelineMessage`, `MessageRole`,
`ErrorSeverity`, `ErrorComponent`, `SessionMode`), `src/pipeline/types.rs`
(`UtteranceSource`),
`src/transport/serve/setup.rs` (mode gating). Wire contract in the
[protocol](../../clients/protocol.md).
