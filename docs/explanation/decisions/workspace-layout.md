# Workspace layout: root core crates and stage-grouped plugins

**Status:** active
**Last verified:** 2026-11-03
**Owner:** Docs
**Decided:** 2026-11

## Context

As the workspace grew to 27 Rust crates (7 core/shared crates and 20 provider
plugin crates across STT, TTS, LLM, S2S, VAD, and Turn Detection), keeping all
27 crates flat at the repository root made it hard for newcomers to distinguish
the core runtime and SDK from individual provider integrations. At the same
time, moving core crates into nested subdirectories would add friction to daily
engine development, Docker volume mounts, and top-level repository navigation.

## Decision

1. **Core & shared crates stay at the repository root:** `voice-pipes`,
   `voice-pipes-sdk`, `voice-pipes-cli`, `voice-pipes-onnx`, `voice-pipes-net`,
   `voice-pipes-bench`, `voice-pipes-gym`, and `voice-pipes-admin`.
2. **Provider plugin crates live under `plugins/<stage>/<name>`:** grouped by
   pipeline stage (`plugins/stt/*`, `plugins/tts/*`, `plugins/llm/*`,
   `plugins/s2s/*`, `plugins/turn/*`, `plugins/vad/*`).
3. **Cargo package names remain `voice-pipes-<stage>-<name>`:** directory paths
   change, but `package.name` in each plugin's `Cargo.toml` stays
   `voice-pipes-<stage>-<name>`, preserving all Cargo feature flags, crate
   imports (`voice_pipes_<stage>_<name>`), `-p` CLI invocations, and `RUST_LOG`
   targets without breaking changes.
4. **Scripts live in one root `scripts/` directory**, and benchmark/dev example
   configs live in `voice-pipes/examples/bench/` and `voice-pipes/examples/dev/`.

## Consequences

- The repository root clearly highlights the core engine, SDK, CLI, and admin
  console, while scaling cleanly to dozens of additional plugins under
  `plugins/<stage>/<name>`.
- Architectural rules (`workspace_rules.rs`) enforce that every plugin crate
  directory matches `plugins/<stage>/<name>` while depending only on
  `voice-pipes-sdk` (and shared leaf crates `voice-pipes-onnx` /
  `voice-pipes-net`).

## Where in code

Root [Cargo.toml](../../../Cargo.toml) (`[workspace.members]` and
`[workspace.dependencies]`), `provider_rel_dir` and `member_dir` in
`voice-pipes-cli/tests/workspace_rules.rs`, and the Docker build globs in
`voice-pipes/Dockerfile` and `voice-pipes/Dockerfile.cuda`.
