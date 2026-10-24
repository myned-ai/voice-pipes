# Development setup

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

What you need on a clean machine to build, test and change the workspace.
Building the CLI to *use* it is covered by the
[quickstart from source](../getting-started/quickstart-source.md); this page
adds the admin console and the conventions.

## Prerequisites

- Rust 1.91+ (2024 edition) via rustup, with `rustfmt` and `clippy`.
- Windows: Visual Studio 2022 Build Tools with the C++ workload. Linux:
  `libasound2-dev` (for the `audio-device` feature, which the default build
  and `batteries` include). macOS: Xcode command-line tools.
- `protoc` is vendored; nothing to install.
- eSpeak NG for Piper TTS at runtime.
- Admin console: Python 3.12 (exactly) and Node 24 or 22.13+; see
  [Admin console › Running it](../guides/admin-console.md#running-it).
- Optional: `cargo-deny` (`cargo install cargo-deny --locked`) to run the
  dependency policy locally; CI runs it anyway.

## Workspace layout

| Crate / dir | What it is |
|---|---|
| `voice-pipes/` | Engine library: pipeline, providers, transport, orchestrator |
| `voice-pipes-sdk/` | Stable plugin SDK, the semver-disciplined trait boundary |
| `voice-pipes-cli/` | The `voice-pipes` binary (composition root) |
| `voice-pipes-bench/` | Benchmarking and automated voice-testing extension |
| `voice-pipes-admin/` | Admin console: React frontend, FastAPI backend |
| `examples/` | Standalone integrations that talk to a worker from outside |
| `docs/` | This documentation; `docs/internal/` is never published |

## Build and test

```sh
cargo build -p voice-pipes-cli
cargo test -p voice-pipes --lib --features "batteries,text-grpc"
cargo test -p voice-pipes-sdk -p voice-pipes-bench -p voice-pipes-cli
cargo clippy --workspace --all-targets --features "voice-pipes/realtime,voice-pipes/text-grpc" -- -D warnings
cargo fmt --all --check
```

The clippy line is exactly what CI runs and it must stay at zero warnings;
plain `cargo clippy --workspace --all-targets` on default features is also
expected to be clean. See [Testing](testing.md) for the full suite list,
including which tests need model files or API keys.

### Admin console

```sh
bash voice-pipes-admin/start-native.sh        # Linux; Windows: .\voice-pipes-admin\start-native.ps1
```

starts the backend on :8000 and Vite on :5173 from the locked dependencies
(Python 3.12 venv in `backend/.venv`, `npm ci`). For tests and lint, add the
dev lock to the same venv:
`backend/.venv/bin/python -m pip install --require-hashes -r backend/requirements-dev.txt`
(`backend\.venv\Scripts\python.exe` on Windows). How the locks are
regenerated is in `voice-pipes-admin/backend/README.md`.

The frontend's production build type-checks; `vite dev` does not, so run
`npm run build` before opening a pull request that touches TypeScript.

## Conventions

- **Style.** `clippy::pedantic` plus selected restriction lints are enabled
  in `voice-pipes/Cargo.toml`. Every `#[allow]` carries a `reason`.
- **No panics in runtime paths.** Factories return errors for bad
  configuration; locks are `parking_lot`; `unwrap()` and `expect()` belong in
  tests. Poisoned-lock panics were removed on purpose; do not reintroduce
  `std::sync::Mutex` in hot paths.
- **Tests next to the code.** Unit tests for `foo.rs` live in `tests/foo.rs`
  in the same directory, wired with `#[path]` so they keep private access;
  module-wide scenario suites in `tests/suite.rs` or `tests/suite/`;
  cross-cutting integration suites in `voice-pipes/tests/`.
- **Features.** A new provider is a Cargo feature; the wizard's
  feature-coverage test checks it is in the compose build args, so add it
  there too. See [Wizard descriptor](../extending/wizard-descriptor.md).
- **Docs.** A change to behaviour, configuration or the wire protocol updates
  the matching page under `docs/`, or the pull request says why there is no
  doc impact. `pwsh scripts/docs-check.ps1` and `bash scripts/leak-check.sh`
  must pass.
- **Changelog.** User-facing changes add a line under `[Unreleased]` in
  `CHANGELOG.md`.

## Adding a provider

Out-of-tree providers implement the [plugin SDK](../extending/plugin-sdk.md)
traits in their own crate; nothing here changes. Built-in providers implement
the trait, register a factory in the stage's registry, get a Cargo feature,
a wizard descriptor, a provider page under `docs/guides/providers/` and, if
they need a key, a line in `voice-pipes/.env.example` and the compose
pass-through.

## Pull requests

Keep them focused and link the issue. CI must pass. Contributions are
licensed under Apache-2.0 like the project (inbound = outbound); third-party
code needs a compatible license and an entry in `NOTICE`. Security issues go
to the address in `SECURITY.md`, not to a public issue.
