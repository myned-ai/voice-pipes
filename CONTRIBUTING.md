# Contributing to Voice Pipes

Thanks for your interest! Bug reports, fixes, providers, docs improvements —
all welcome.

## Getting started

Prerequisites: Rust 1.91+ (2024 edition). `protoc` is vendored via
`protoc-bin-vendored` — no system install needed. The admin console
additionally needs Node 20+ and Python 3.11+.

```sh
git clone <repo-url> && cd <repo>
cargo build -p voice-pipes-cli          # the `voice-pipes` binary
cargo test -p voice-pipes --lib --features batteries,text-grpc
cargo test -p voice-pipes-sdk -p voice-pipes-bench
```

For the full containerized stack, see the [README install section](README.md#install).
Repository layout is described in the [README](README.md#repository-layout).

## Making changes

- **Read the [engineering rules](docs/contributing/engineering-rules.md).**
  They are short, each says which bug it prevents and what checks it, and the
  checklist at the end is what a review goes through.
- **Match the existing style.** The workspace runs `clippy::pedantic` plus
  selected restriction lints (`[workspace.lints]` in the root `Cargo.toml`,
  which every crate inherits); run
  `cargo clippy --all-targets` and `cargo fmt` before pushing.
- **Add tests** next to the code you change. Unit tests for `foo.rs` live in
  `tests/foo.rs` in the same directory (for `mod.rs`, the directory's name),
  declared from the source file as

  ```rust
  #[cfg(test)]
  #[path = "tests/foo.rs"]
  mod tests;
  ```

  so the test file stays a child module with private access (`use super::*`).
  A module-wide scenario suite lives in `tests/suite.rs`, or `tests/suite/`
  split by subject with shared fixtures in `common.rs`. Integration tests that
  only need the public API go in `voice-pipes/tests/`.
- **Avoid panics in runtime paths.** Factories return errors for bad config;
  locks use `parking_lot`; `.unwrap()`/`.expect()` belong in tests.
- **Docs:** if you change behavior, config, or the wire protocol, update the
  matching page under `docs/` (see `docs/contributing/docs-style.md`), and add
  a line under `[Unreleased]` in `CHANGELOG.md` for user-facing changes.

## Adding a dependency

Every crate in the build is checked by CI, so a new dependency needs:

- **One version.** Declare it once in the root `Cargo.toml`'s
  `[workspace.dependencies]` and inherit it (`name.workspace = true`). A
  second version of a crate already in the graph fails `cargo deny`; the
  exceptions in `deny.toml` are third-party and each names who needs it.
- **An allowed license and no open advisory** (`cargo deny check`).
- **An audit.** `cargo vet` must pass: most common crates are covered by the
  imported audit sets; for one that is not, review it and run
  `cargo vet certify`, or add an exemption with the reason in
  `supply-chain/config.toml`.
- **A reason to be always on.** If the engine needs it only for some
  providers, put it behind their feature. `scripts/dep-budget.py` fails CI
  when the engine with no features grows past its budget.

## Adding a provider

STT/TTS/text providers are trait-based. External providers implement the
`voice-pipes-sdk` traits in their own crate — no changes here required (see
`docs/extending/plugin-sdk.md`). Built-in providers implement the trait
and register a factory; the wizard entry is one descriptor in
`voice-pipes/src/commands/config_wizard/providers.rs`.

## Pull requests

- Keep PRs focused; link the issue they address.
- CI must pass. PRs run the core test suites on Ubuntu.
- By contributing, you agree that your contributions are licensed under the
  [Apache License 2.0](LICENSE), the same license as the project
  (inbound = outbound). Keep third-party code out unless its license is
  compatible and attributed in [NOTICE](NOTICE).

## Reporting security issues

Please do **not** open a public issue — see [SECURITY.md](SECURITY.md).
