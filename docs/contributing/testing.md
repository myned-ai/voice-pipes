# Testing

**Status:** active
**Last verified:** 2026-10-11
**Owner:** Docs

What the test suites are, which ones need models or API keys, and how CI
runs them.

## The suites

| Suite | Command | Needs |
|---|---|---|
| Engine unit tests (runner, gateway, pipeline, realtime, text, VAD, agent link) | `cargo test -p voice-pipes --lib --features "batteries,text-grpc"` | nothing |
| Engine integration: transport, OpenAI-compatible LLM | `cargo test -p voice-pipes --test transport_integration --features "batteries,text-grpc"`; `cargo test -p voice-pipes-llm-openai-compat` | nothing |
| Pipeline integration (Parakeet + Piper end to end) | `cargo test -p voice-pipes-cli --test pipeline_integration -- --ignored` | model files on disk; the tests are `#[ignore]`d and say which |
| Plugin SDK | `cargo test -p voice-pipes-sdk` | nothing; doc examples compile as `no_run` |
| Benchmark crate | `cargo test -p voice-pipes-bench` | nothing |
| Synthetic caller against a live provider | `RUN_SYNTHETIC_CALLER=1 cargo test -p voice-pipes-bench --features synthetic-caller -- --ignored` | `OPENAI_API_KEY` |
| CLI smoke tests | `cargo test -p voice-pipes-cli` | nothing |
| Quickstart & example configs verification | `cargo test -p voice-pipes-cli --test quickstart_and_examples -- --nocapture` | validates every example config, checks docs commands, and prints the runtime readiness & skip report |
| Admin frontend (client SDK FSM, session core, components) | `cd voice-pipes-admin/frontend && npm run lint && npx tsc -b && npx vitest run` | Node 24 or 22.13+ (`npm` refuses others) |
| Admin backend (report engine, health verdicts) | `cd voice-pipes-admin/backend && .venv/bin/ruff check . && .venv/bin/python -m pytest -q app` | Python 3.12 in `.venv`, installed with `--require-hashes` from `requirements.txt` and `requirements-dev.txt`; the golden replay needs a capture corpus holding the golden sessions (`VP_GOLDEN_CAPTURES_DIR`) and skips, saying why, without one |

## Quickstart & Example Configs Verification

Whenever modifying configuration schemas, CLI commands, or documentation, run:

```sh
cargo test -p voice-pipes-cli --test quickstart_and_examples -- --nocapture
```

### What it verifies
1. **Schema & Syntax of Every Example:** Scans all `.toml` files in `voice-pipes/examples/` and ensures they parse cleanly against `PipelineConfig`, `WorkerConfig`, or `OrchestratorConfig`.
2. **Quickstart Command Sync:** Dynamically parses `docs/getting-started/quickstart-source.md` and verifies all documented CLI command lines match clap arguments.
3. **Standalone Tools (TTS & STT):** Synthesizes a real audio file with Piper and validates its RIFF headers and byte size.
4. **Hosted S2S Credentials Gate:** Verifies that cloud Speech-to-Speech (e.g. Gemini Live) cleanly validates options and enforces API keys without crashing or hanging.
5. **Runtime Readiness & Skip Diagnostics Report:** Evaluates all 60 pipeline configurations and prints a structured diagnostics table showing which are `[READY]` on your current machine, and if `[SKIPPED]`, exactly why:
   - `Missing API key: <ENV_VAR>` (e.g. `GEMINI_API_KEY`, `OPENAI_API_KEY`)
   - `Missing model: <path>` (e.g. `models/kokoro-v1.0.onnx`, run `voice-pipes models fetch`)
   - `Feature '<name>' not compiled in build` (e.g. `--features tts-kokoro`)

## What CI runs

`.github/workflows/core-tests.yml` runs on every push and pull request that
touches a crate, the admin console, the docs or the scripts. Its jobs, by the
names GitHub shows:

| Job | What it does |
|---|---|
| docs gate (links, plan index, leak check) | `pwsh scripts/docs-check.ps1` (broken links and heading anchors, PLAN-STATUS completeness), `bash scripts/leak-check.sh` (private names, personal paths, account ids) and `python3 scripts/compose-check.py` (no host-port collisions; sidecars and debug APIs on loopback). |
| rustfmt + clippy | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --features "voice-pipes/realtime,voice-pipes/text-grpc" -- -D warnings` (the zero-warning baseline dates from 2026-08; keep it); `cargo check -p <crate>` for every library crate on its own, so a crate cannot lean on a feature another workspace member enables; `cargo deny check` (licenses, advisories, sources, bans) over the all-features graph. |
| workspace tests | The test suites and the no-skip gate; step by step below. |
| clippy + engine tests with the worker images' features | Reads the CPU image's feature list from `docker-compose.yml` (`scripts/image-features.py`), then runs clippy with `-D warnings` and `cargo test` for the CLI and the engine with those features plus the gRPC component features and `turn-vap`, so code only an image enables is compiled before merge. |
| engine with no providers (build, tests, crate budget, workspace rules) | Clippy and tests for `voice-pipes` and `voice-pipes-cli` with `--no-default-features`, with no ALSA headers installed (a build that needs audio devices without the `audio-device` feature fails); the bench crate's features on their own; `scripts/dep-budget.py` (crates per build shape); `cargo test -p voice-pipes-cli --no-default-features --test workspace_rules`. |
| the CPU image builds and registers what CI tested | Builds the engine image as the release does and compares `voice-pipes inspect --format json` from the image with a native build of the same features (`scripts/compare-inspect.py`). |
| the declared minimum Rust version builds every crate | `cargo check --workspace --all-targets` on the `rust-version` from `Cargo.toml`. |
| the release binary builds on windows-latest, macos-latest | `cargo check -p voice-pipes-cli` with its default features. |
| every dependency audited or exempted (cargo vet) | `cargo vet --locked`. |
| SDK API compatibility | `cargo semver-checks -p voice-pipes-sdk` against the base branch (`main` on a push). |
| every crate builds from its published files | `cargo publish --workspace --dry-run`; uploads nothing. |
| vitest client SDK FSM | The client SDK tests (FSM and session core) alone, on Node 22. |
| admin backend (ruff, pytest) | On Python 3.12 (the job fails on any other), installed with `--require-hashes` from `requirements.txt` and `requirements-dev.txt`, then `pip check`, `ruff check .` (rule set in `voice-pipes-admin/backend/ruff.toml`; a deliberate broad `except` carries `# noqa: BLE001` and the reason) and `pytest -q -rs app`. The golden health replay skips, saying so, without a capture corpus; it is outside the no-skip gate below. |
| admin frontend on Node 22 / 24 (lint, tsc, vitest, build) and admin images | On each Node version `package.json` admits: `npm ci`, `npm run lint`, `npx tsc -b`, `npx vitest run`, `npm run build`. On Node 22 only, both admin images are built and the frontend image's security headers (`X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`, `Cache-Control`) are checked live with `curl -I`. |

The `workspace tests` job runs these steps in order. Every test step appends
its output to one file, which the gate near the end reads:

1. `cargo test -p voice-pipes --lib --tests --features "batteries,text-grpc"`:
   the engine's unit tests and every target in `voice-pipes/tests/`, so a new
   suite is picked up without editing the workflow. The model-backed tests
   are `#[ignore]`d and are not run.
2. `python3 scripts/each-test-alone.py config_wizard -- -p voice-pipes --lib --features "batteries,text-grpc"`:
   every wizard test in a fresh process of its own, then the set once more
   together. The plugin registries are process-wide; a test that passes only
   because another test registered something first fails here.
3. `cargo test --workspace --exclude voice-pipes --exclude voice-pipes-cli --exclude voice-pipes-bench`:
   the SDK, the shared crates and every plugin crate, by exclusion, so a new
   crate is tested the day it joins the workspace.
4. `cargo test -p voice-pipes-sdk --lib -- --ignored espeak` with espeak-ng
   installed: the phonemiser tests that are `#[ignore]`d for machines without
   the program.
5. `cargo doc --no-deps` with `RUSTDOCFLAGS=-D warnings` over the crates of
   step 3: a link to a private or missing item is an error.
6. `cargo test -p voice-pipes-bench --features turn-vap`, then
   `cargo test -p voice-pipes-cli`.
7. `no test passed by skipping`: fails if the kept output holds
   `test result: FAILED` or `error: test failed` (the `tee` in the steps above
   hides their exit codes), or the word "skipping" or "skipped" in any case.
   A test that prints "skipping" and returns has passed without running; make
   it `#[ignore]` and fail on the missing prerequisite (rule T1 in
   [Engineering rules](engineering-rules.md)). Only this job's output is
   scanned; the `bare-engine` and `image-features` jobs' test output is not.
8. `python3 scripts/gen-reference.py --check --bin target/debug/voice-pipes`:
   renders `docs/reference/cli.md`, `cargo-features.md` and `providers.md`
   from the code and compares them with the committed pages; also checks the
   hand-written reference for providers, events, features and config drift.

Other workflows:

- `tts-bench-smoke.yml`, on changes to the bench crate, the transport or the
  benchmark panel: `cargo fmt --check` on the bench and CLI crates, the CLI
  built with `tts-pocket,tts-piper`, the bench crate's lib tests and its
  `tts_benchmark_smoke` suite, the transport codec tests,
  `scripts/benchmark-boundary-check.ps1` (the core stays benchmark-free), and
  vitest and eslint on the TTS benchmark panel components (Node 22).
- `docs.yml`, on a push to `main`: builds the mdBook and the SDK rustdoc and
  deploys them to GitHub Pages. It is not a gate.
- `release.yml`, `publish.yml` and `images.yml` run on a version tag and first
  require a green `core-tests` run on that commit
  (`scripts/require-green-ci.sh`); see [Release process](release-process.md).

CUDA features cannot build on public runners and are not covered by CI.
`cargo clippy --workspace --all-targets` on default features is also expected
to be clean; contributors often run that one.

## Conventions

- Unit tests for `foo.rs` live in `tests/foo.rs` in the same directory (for a
  `mod.rs`, the file is named after the directory), declared from the source
  file with `#[cfg(test)] #[path = "tests/foo.rs"] mod tests;` so the test
  file is a child module and `use super::*` reaches private items. A
  module-wide scenario suite lives in `tests/suite.rs`, or in `tests/suite/`
  split by subject with shared fixtures in `common.rs`. Cross-cutting
  integration suites live in `voice-pipes/tests/`.
- A test that needs a model file is `#[ignore = "…"]` with a reason that names
  the model and where it lives (the Nemotron 3.5 provider tests say
  `needs the Nemotron 3.5 model in voice-pipes/models/nemotron-3.5`; the CLI's
  `pipeline_integration` suite says `requires model files on disk` and its
  module doc names the Parakeet and Piper files under `voice-pipes/models/`).
  Asked for with `-- --ignored` on a machine without the model, it fails and
  says what is missing: the Nemotron tests panic naming the download script
  and the `NEMOTRON35_MODEL_DIR` override; the pipeline tests panic with the
  provider build error. A test that notices a missing model, prints
  "skipping" and returns has passed without running; that is forbidden (rule
  T1 in [Engineering rules](engineering-rules.md)), and the
  `no test passed by skipping` CI step fails on the word.
- A test that needs a paid API is gated three ways: a Cargo feature, `#[ignore]`,
  and an environment variable. It must not run by accident.
- Runtime paths do not panic. Factories return errors for bad config; locks
  are `parking_lot`; `unwrap()` and `expect()` belong in tests.

## Golden fixtures

The gateway's control-frame ordering per turn is contractual and pinned by a
golden trace. If a change legitimately alters the ordering, regenerate the
fixture on purpose after reviewing the diff:

```sh
UPDATE_GOLDEN=1 cargo test --features realtime transport::serve::tests::golden_classic_happy_path_control_trace
```

A failing golden test with no intended protocol change is a regression; do
not regenerate to make it pass. The admin backend's health verdicts have the
same discipline with `UPDATE_HEALTH_GOLDEN=1`.

## Benchmarks as regression tests

The bench crate writes diffable `results.json` files (p50/p90/p99 end-to-end,
TTS time to first byte, barge-in round trip, word error rate). Functional
fields are reliable pass/fail; latency is only meaningful as an A/B against a
baseline with roughly 25–30% tolerance. See
[Benchmarking and evals](../guides/benchmarking-and-evals.md).
