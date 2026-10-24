# SDK stability policy

**Status:** active
**Last verified:** 2026-10-05
**Owner:** Docs

What `voice-pipes-sdk` 1.x promises a plugin author: which changes can
arrive in a minor release, which wait for 2.0, and how that is checked.
The rest of the workspace (the engine, the CLI, the provider crates) moves
in lockstep at its own version and makes no such promise; build plugins
against the SDK, not the engine. Two shared crates sit between: a plugin
that reaches the network or opens an ONNX model is required to use them,
and they are not yet 1.0 ([below](#the-shared-crates-voice-pipes-net-and-voice-pipes-onnx)).

## What is covered

Everything public in `voice-pipes-sdk`: the items reachable from the crate
root, their documented behaviour, and the contract each trait states (what
the engine calls, in what order, and what it expects back). The `testing`
module and its helpers are covered too. Private items, `#[doc(hidden)]`
items and the exact text of error and log messages are not.

## What a release may change

The SDK follows [Cargo's semver rules](https://doc.rust-lang.org/cargo/reference/semver.html).

A **patch** release (1.2.3 to 1.2.4) fixes a bug without changing an API.

A **minor** release (1.2 to 1.3) only adds:

- new types, functions, modules and `with_*` setters;
- new trait methods **with a default implementation**, which existing
  implementations keep working with;
- new variants of an enum: every public enum is `#[non_exhaustive]`, so a
  `match` on one needs a `_ =>` arm, and a new variant does not break it;
- new fields on a struct marked `#[non_exhaustive]`, which plugins build
  with a constructor or `Default` and the `with_*` setters, never a struct
  literal;
- a higher minimum Rust version (see below);
- deprecations: an item is marked `#[deprecated]` with its replacement for
  at least one minor release before 2.0 removes it.

A **major** release (2.0) is needed for anything else: removing or
renaming an item, changing a signature, a new trait method without a
default, a behaviour change that breaks a stated contract, a new field on a
struct that is not `#[non_exhaustive]`, or a major version of a dependency
the SDK exposes.

### Structs whose fields are complete

These structs are not `#[non_exhaustive]`, so plugins can build and match
them with literals; their fields are what the thing is, and adding one is a
major release:

| Struct | Fields |
|---|---|
| `AudioFrame` | samples, sample rate, channels |
| `AudioChunk` | samples, whether it ends the utterance |
| `VadResult` | speech probability, whether the chunk is speech |
| `LatencyStats` | p50, p90, p99, mean, min, max |
| `WordBoundary` | word, start, duration |
| `ProviderSchema` | descriptors, current values |
| `TraceEvent` | timestamp, component, event type, payload |

Every other struct with public fields is `#[non_exhaustive]`.

## Dependencies the SDK exposes

Some types in the SDK's API come from other crates, so their major versions
are the SDK's too:

| Crate | Version | Where it shows |
|---|---|---|
| `tokio` | 1 | `mpsc` channels in the streaming traits |
| `tokio-util` | 0.7 | `CancellationToken` (re-exported) |
| `async-trait` | 0.1 | the `#[async_trait]` attribute (re-exported) |
| `serde_json` | 1 | `Value` in options and payloads |
| `arc-swap` | 1 | live string parameters (`ArcSwap<String>`) |
| `anyhow` | 1 | `BuildError` converts from `anyhow::Error` |

`tokio-util` and `async-trait` are not at 1.0. The SDK stays on
`tokio-util` 0.7 and `async-trait` 0.1 for all of 1.x: moving to a new
0.x of either is a breaking change and waits for SDK 2.0. Name them
through the SDK's re-exports (`voice_pipes_sdk::CancellationToken`,
`voice_pipes_sdk::async_trait`) so a plugin always has the version the
SDK uses.

## Minimum Rust version

The SDK builds with the workspace's `rust-version` (1.91 today). A minor
release may raise it, never a patch release; the changelog says so when it
does.

## The shared crates: voice-pipes-net and voice-pipes-onnx

A provider that talks to a network service does it through
`voice-pipes-net` (one TLS stack, bounded clients, the endpoint check for
credentials), and one that runs an ONNX model opens it through
`voice-pipes-onnx` (one session opener, the pool, the mel front end); the
workspace's own rules hold its providers to both. Neither is covered by
this policy yet. They are at 0.1, move in lockstep with the engine, and
follow Cargo's pre-1.0 rule: a 0.1.x release is compatible, and 0.2 may
break.

- Depend on them as `"0.1"`; Cargo's default requirement keeps a plugin on
  0.1.x and never moves it to 0.2 by itself.
- What they offer is their crate root and nothing else:
  - `voice-pipes-net`: `http_client` and `HttpOptions`, `sensitive_header`,
    `error_body`, `limited_body` and `BodyError`, `ERROR_BODY_LIMIT`,
    `credential_endpoint_error`, `check_credential_endpoint` and
    `EndpointError`, `ALLOW_INSECURE_OPTION`, `BoundedLines` and
    `LineTooLong`, `install_crypto_provider`, the `reqwest` re-export, and
    with feature `ws` `connect_websocket`, `websocket_config`, `WebSocket`,
    `ConnectError` and the `tokio_tungstenite` re-export; feature `testing`
    adds the loopback servers in `testing`.
  - `voice-pipes-onnx`: `SessionPool`, `PooledSlot`, `PoolExhausted`,
    `EmptyPool`, `DEFAULT_POOL_WAIT`, `argmax`; with feature `session`
    `open_session`, `SessionOptions`, `SessionOpenError`, `ExecutionTarget`,
    `EXECUTION_PROVIDER_VAR`, and `dim`, `nth_output` and
    `ModelOutputError` for reading a run's outputs; with feature `mel` the
    `mel` module.
- A breaking change to either is called out in the changelog under
  **Changed** with a migration line, as `argmax` returning an `Option` was.

They come under this policy at their own 1.0, once their surface has held
still through a release or two.

## Engine and plugin compatibility

A plugin built against SDK 1.y works with an engine built against any SDK
1.z with z ≥ y: Cargo resolves one SDK 1.x for the binary, and a minor
release only adds. The voice-pipes binary links its plugins at build time,
so the check happens there, as a compile error rather than a failure at run
time.

## How it is checked

- `cargo semver-checks` runs on every pull request against the base branch
  (CI job *SDK API compatibility*) and fails a breaking change whose version
  does not say so.
- `#![warn(missing_docs)]`: every public item states its contract.
- The engine's tests register mock plugins from outside the engine crate,
  through the same API a plugin uses.

## Before 1.0

Versions 0.1 to 0.6 followed Cargo's pre-1.0 rule (a `0.y` bump could
break). The changelog records what each changed.
