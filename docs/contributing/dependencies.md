# Dependencies

**Status:** active
**Last verified:** 2026-10-17
**Owner:** Engine

How third-party crates enter the workspace, and why one of them is a release
candidate. The rules themselves are S9 and S10 in
[Engineering rules](engineering-rules.md#s-security-and-untrusted-input).

## The basics

- A dependency is declared once, under `[workspace.dependencies]` in the root
  `Cargo.toml`, and members take it with `.workspace = true`.
- It has an allowed licence and no open advisory (`cargo deny check`), and an
  audit or a recorded exemption (`cargo vet`).
- `Cargo.lock` is committed, and every build that should reproduce the tested
  one reads it: CI, the Dockerfiles, and `cargo install --locked`. Without
  `--locked`, `cargo install` ignores the lock file and resolves every
  dependency afresh, to versions nobody here has tested.

## Pre-release versions are pinned exactly

For a pre-release, Cargo reads `"2.0.0-rc.12"` as "rc.12 or any later
pre-release of 2.0.0". Release candidates make no compatibility promise, so a
fresh resolve can pick one that does not compile. On 2026-10-17 an install
without `--locked` took `ort` rc.14, whose `Session::builder` takes an
`Environment`, and the build failed.

So every pre-release requirement is exact: `version = "=2.0.0-rc.12"`. The
workspace rule `pre_release_dependencies_are_pinned_exactly` (in
`voice-pipes-cli/tests/workspace_rules.rs`) fails the build otherwise, for the
root manifest and every member, in normal, dev, build and target-specific
dependencies.

## Why `ort` is a release candidate

`ort` is the Rust binding to Microsoft's ONNX Runtime, which runs every ONNX
model here: Parakeet, Moonshine, Nemotron, Silero, Piper, Kokoro and the turn
detectors. Its 2.0 line has been a release candidate since February 2024; the
last stable release, 1.16.3 (November 2023), binds ONNX Runtime 1.16, has a
different API and is no longer maintained. 2.0 is what the Rust ecosystem
uses.

The release-candidate label is about the binding's API, which still changes
between candidates. The inference itself runs in ONNX Runtime's own release
binaries, which `ort` downloads at build time: rc.12 fetches ONNX Runtime
1.24.2.

Alternatives considered and rejected:

| Option | Why not |
|---|---|
| `ort` 1.16.3 | A three-year-old runtime, unmaintained, and a rewrite of every ONNX provider to the old API. |
| `tract` (pure Rust, stable releases) | No CUDA, slower on CPU, and operator coverage would have to be proven model by model. |
| Binding ONNX Runtime ourselves | Needs `unsafe` FFI, which every crate here forbids. |

## Upgrading `ort`

An upgrade is its own branch and its own change, never part of a Dependabot
group:

1. Change the exact version in the root `Cargo.toml` and run
   `cargo update -p ort --precise <version>` (and `ort-sys` with it).
2. Fix the API changes; they land mostly in `voice-pipes-onnx` and in the
   provider crates that call `ort` directly.
3. Run the full suite, including the `#[ignore]`d model tests with the models
   on disk, and `cargo build -p voice-pipes-cli --features ort-cuda` for the
   CUDA paths.
4. Build the CPU and CUDA images and run `voice-pipes check` on an ONNX
   pipeline in each.
5. `cargo deny check`, `cargo vet`, and `scripts/dep-budget.py`.

Held candidates: rc.13 makes `ort::ep::CUDA` require `ort`'s `cuda` feature,
which breaks `voice-pipes-onnx`'s CPU-build test of a CUDA provider that
cannot register; rc.14 also makes `Session::builder` take an `Environment`.
