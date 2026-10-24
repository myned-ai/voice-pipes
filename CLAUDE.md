<!-- Mirrors AGENTS.md for Claude Code CLI -->
# Voice Pipes — Claude Instructions

Refer to [AGENTS.md](AGENTS.md) for full architecture, lint rules, commands, and plugin authoring guides.

## Quick Cheat Sheet
- **Check Workspace**: `cargo check --workspace`
- **Lint**: `cargo clippy --workspace -- -D warnings`
- **Architectural Rules**: `cargo test -p voice-pipes-cli --test workspace_rules`
- **Audit**: `cargo deny check`
- **Backend Tests**: `$env:PYTHONPATH="."; py -3.12 -m unittest discover -s app -p "test_*.py" -t .` (in `voice-pipes-admin/backend`)
- **Frontend Tests**: `npm run lint && npx tsc -b && npm test` (in `voice-pipes-admin/frontend`)

## Critical Invariants
1. `#![forbid(unsafe_code)]` on all Rust crates.
2. Zero Clippy warnings (`-D warnings`).
3. Plugins depend ONLY on `voice-pipes-sdk 1.x`, never on `voice-pipes`.
4. Audio pipeline is strictly 16 kHz 16-bit mono PCM.
5. All subprocess spawns (`Command::new`) must sanitize arguments and enforce an execution timeout with process termination.
6. **Metrics are the product; a wrong metric is a bug.** Per-component observability is Voice Pipes' main feature, so:
   - A metric name has one definition (start anchor, end anchor, and what is inside: queue/lock wait, connect, prompt ingest, retries) in `docs/reference/metrics-glossary.md`, and it means the same for every provider.
   - Critical-path latency is measured by the engine at the trait seam, with the same clock for every provider. A provider-reported number is a breakdown that must fit inside the engine's span, never a replacement for it.
   - Missing is `null`, never `0`. A value that cannot be true is a measurement fault: the report flags it as one instead of blaming the next component.
   - A new provider or metric ships with a conformance test using a deliberately slow fake (slow connect, prompt ingest, first chunk) that proves the delay appears in the metric.
   - After changing a pipeline or a provider, check one real session report's numbers against its raw events before calling the work done.
