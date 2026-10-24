# Provider factory registry

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-04

## Context

The configuration module owned the TOML types, thirty default functions,
the CLI merge, validation with hard-coded provider name lists, and factory
functions that imported and constructed every concrete provider behind
feature gates. Adding one STT provider meant five edits in that file plus the
new module.

## Decision

Each stage has a factory trait (`SttFactory`, `TtsFactory`,
`TextProcessorFactory`, `VadFactory`, turn-detector and audio post-processor
factories) with `name()`, optional `aliases()`, `is_local()`, `validate()`
and `build()`. A registry function per stage returns the compiled-in
factories; configuration resolves `provider = "…"` by name against it.
Validation is registry-driven: an unknown name is whatever the registry does
not contain, not a hard-coded list. External crates register their factory
with one call before the engine starts; the same mechanism the wizard's
descriptor tables are tested against.

## Consequences

- A new built-in provider is its module plus one line in a registry vec.
- An out-of-tree provider needs no change in this repository, which is what
  makes the plugin SDK a real boundary.
- Aliases exist (`nemotron` for `nemotron-streaming`, `google-stt` for
  `google`); the wizard's tests catch the name-versus-alias trap.

## Where in code

`voice-pipes/src/stt/mod.rs` (`stt_factories`, `register_stt_factory`),
`src/tts/mod.rs`, `src/text/mod.rs`, `src/vad/mod.rs`,
`src/config/factories.rs`, `src/config/validate.rs`; the SDK traits in
`voice-pipes-sdk/src/`. Guide: [Plugin SDK](../../extending/plugin-sdk.md).
