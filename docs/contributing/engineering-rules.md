# Engineering rules

**Status:** active
**Last verified:** 2026-10-01
**Owner:** Engine

The rules code in this repository is held to, and why. Each one exists because
breaking it caused a real bug or a real review round here; the "prevents"
line says which. They apply to the engine, the SDK, the shared crates and
every provider crate, in-tree or not.

How to use this page:

- **Writing code:** read the section for what you are touching. The
  [checklist](#before-you-open-a-pull-request) at the end is the short form.
- **Reviewing:** cite a rule by its id (`S3`, `A2`) in the review. Do not put
  rule ids, issue numbers or plan names in code comments; a comment explains
  the mechanism, and the history lives in git.
- **Held by** says what stops a violation today: a named CI check, or
  *review*. A rule held only by review that is broken twice gets a check.
  Adding that check is part of the fix, not a follow-up.

Related pages: [Testing](testing.md) for how to run the suites,
[Documentation style](docs-style.md) for docs, and
[Plugin SDK](../extending/plugin-sdk.md) for the provider recipe.

---

## B. Boundaries

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| B1 | A component lives in the engine only if every pipeline needs it. Every vendor or model is its own crate. | Adopters carrying code and dependencies for providers they do not use. | `scripts/dep-budget.py` (bare-engine budget) |
| B2 | A provider crate depends on `voice-pipes-sdk`, `voice-pipes-onnx`, `voice-pipes-net` and its own third-party crates. Never on the engine, never on another provider. | A provider pulling the whole runtime into whoever uses it. | workspace rules: `providers_depend_only_on_the_sdk_and_shared_infrastructure` |
| B3 | The engine names no provider: no vendor name, sample rate, default model or option key in engine code. If the engine needs a fact about a provider, it asks the factory. | Wrong facts for every provider the engine did not special-case (a TTS rate guessed by name was wrong for three of four). | `the_engine_and_the_contracts_depend_on_no_provider`; `the_engine_names_no_provider` (no string literal in engine library code equals a name or alias a provider factory registers; the engine's own `llm` excepted); review for rates and keys |
| B4 | Only the composition crate (`voice-pipes-cli`) links providers. Any other crate that must is listed, with the reason. | Hidden second composition roots. | `only_listed_members_link_a_provider` |
| B5 | A provider crate's public API is `factory()`, `wizard_choices()`, and what its own examples, tests or the bench use. Everything else is `pub(crate)`. | Accidental API that becomes a compatibility promise. | `providers_expose_only_the_listed_api` (every public module, item and re-export, read as syntax); plugin docs built with warnings as errors |
| B6 | Registration never depends on order. A name or alias already taken is an error, not a shadow; menus sort by an explicit rank. | Behaviour that changes with link order; a provider silently replaced by another. | registry refuses duplicates (engine tests); `every_provider_is_wired_into_the_binary` |
| B7 | One implementation. The second crate that needs a helper moves it to the SDK, `voice-pipes-net` or `voice-pipes-onnx`; it does not copy it. | Three WAV parsers with three different bugs; three session openers with two GPU policies. | review; `providers_open_onnx_sessions_through_the_shared_opener` for sessions (any spelling, in tests, examples and benches too) |

## C. SDK contracts

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| C1 | Every factory builds through `build(cfg, ctx: &BuildContext)`. A new input is a field on the context, never a second method. | A trait method per new input, mirrored by hand in every adapter. | SDK semver check; review |
| C2 | Public enums, and structs that the host fills or that will grow, are `#[non_exhaustive]` with `new(required…)` and `with_*` setters. Plain value types that are complete by nature (an audio frame) may stay literals. | Every added field or variant breaking every plugin. | SDK semver check; `the_sdk_enums_can_grow` in `voice-pipes-cli/tests/workspace_rules.rs` for enums; review for structs |
| C3 | A new trait method has a default. | Existing plugins failing to compile on a minor version. | SDK semver check |
| C4 | The SDK carries no engine type and no `dyn Any` escape hatch that reaches one. What a plugin may do to a session is a method on the handle it is given. | A plugin recovering powers the design withheld. | engine test: a plugin's handle yields no engine type; review |
| C5 | Errors say what kind they are. A caller must be able to tell a permanent failure (bad key, unknown voice, bad config) from one worth retrying, and a rate limit carries how long to wait. A change that introduces a kind classifies every existing place the error is built, in the same change; a default kind must not quietly change what a caller does. | Retrying a bad API key; treating a dropped connection as a parse error; a new default kind that switched off retries nobody looked at. | review |
| C6 | Options are read through the SDK's typed readers, which tell a wrong type from an out-of-range value, and every factory refuses keys it does not read. `build` runs the same checks as `validate`. | A misspelt or mistyped setting silently becoming the default; a config that validates but builds differently. | each crate's settings tests; shipped-config test in the CLI; the CLI's factory check (`testing::build_refuses_what_validate_refuses` on every linked factory) |
| C7 | What a schema declares is what the code does. An option shown as live-editable is adopted from the parameter store; one read once at build is marked restart-required; one that is not read is not declared. | A console control that changes nothing. | schema-against-parser tests in the crates that have them; review |
| C8 | A new plugin kind in the SDK ships with: the build context, non-exhaustive types, engine-resolved credentials, and an engine test that registers a mock from outside through the public API and drives the behaviour the engine keys on. | Shipping a contract and breaking it in the next review. | [Plugin SDK §11.4a](../extending/plugin-sdk.md); review |

## S. Security and untrusted input

Everything a vendor, a model file or a client sends is untrusted.

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| S1 | A credential is a `Secret`, resolved by the host and handed to the provider. It is exposed once, at the call that sends it. Provider code does not read the environment for credentials. | Keys in `Debug` output, config hashes and panic messages. | `Secret` type; `providers_take_credentials_from_the_host` in `voice-pipes-cli/tests/workspace_rules.rs` |
| S2 | Logs and errors never contain a payload: no vendor frame, transcript, prompt, model output or URL with a query. Log a length, a kind and a position (`secrets::json_error`). `#[instrument]` says `skip_all` and names its `fields`. | User speech and keys in warn-level logs. | workspace rules `log_lines_carry_no_payload` (event macros at every level: fields, shorthand fields, inline and positional format arguments named for a payload) and `spans_record_no_argument_by_default`; review for errors and for a payload under an unrelated name |
| S3 | Providers reach the network only through `voice-pipes-net`: its client, its WebSocket config, its endpoint check. No direct HTTP or WebSocket dependency. | A provider undoing the redirect, timeout or TLS policy. | `clippy.toml` disallowed methods (every client constructor, every websocket connect but `voice_pipes_net::connect_websocket`, `reqwest::get`); `providers_reach_the_network_only_through_the_net_crate`; `no_crate_enables_the_blocking_http_client` |
| S4 | Every endpoint a config can override is checked before a credential is sent to it: plain HTTP only to a local host or with the explicit opt-in. | A token sent in clear to an operator-supplied host. | endpoint tests in the hosted crates that have them; review |
| S5 | Every read from the network is bounded in size **and** in time. A streamed response is bounded per read, never by a whole-request timeout. | Unbounded buffers; a long spoken reply cut off at the request timeout; a trickling server holding a task forever. | `voice-pipes-net` helpers (`error_body`, `limited_body`, `BoundedLines`); `HttpOptions` offers only a whole-request bound or, for a streamed response, a per-read bound (`streaming(idle)`), so no client is built without a time bound; review for which of the two a caller picks |
| S6 | Parsing external data never panics. No `unwrap`, `expect`, slice or map indexing, or `as` cast on a value from a vendor, a model file or a client. Sizes are computed with checked arithmetic; a zero or negative rate, length or dimension is refused at the boundary. Text is cut at character boundaries: a position found by a character test is never moved by a byte count (`p + 1` after `rfind(char::is_whitespace)`). | A worker crash from one malformed frame or a wrong model export; a reply with a no-break space that panicked the sentence splitter. | every provider library denies `clippy::indexing_slicing`, `unwrap_used`, `expect_used` and `cast_possible_truncation` outside tests (`every_provider_denies_the_panicking_parsing_lints`); malformed-input tests in every hosted crate; review |
| S7 | A limit is enforced where data enters the engine, not in a helper a provider may skip. | An out-of-tree provider bypassing the cap. | engine test with a provider that skips the helper |
| S8 | Provider crates contain no `unsafe`. | Memory-unsafe vendor code in the worker. | `every_provider_forbids_unsafe_code` (the crate attribute, and no `unsafe` in tests, examples or benches) |
| S9 | A dependency is declared once in the workspace, has an allowed licence, no open advisory, and an audit or a recorded exemption. | Version drift, unreviewed code, licence surprises. | `cargo deny`, `cargo vet`; review for declaring a dependency in the workspace (no workspace rule checks it) |
| S10 | A pre-release dependency is pinned exactly (`"=2.0.0-rc.12"`), and upgraded on its own branch with the full suite. See [Dependencies](dependencies.md). | A fresh resolve (`cargo install` without `--locked`) picking a later release candidate that does not compile: `ort` rc.14 changed `Session::builder`. | workspace rule `pre_release_dependencies_are_pinned_exactly` |

## A. Async and streaming correctness

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| A1 | No lock is held across an `.await`, across a model run, or while calling code you do not own (a constructor, a callback, a plugin). Copy what you need out, drop the guard, then call. | A registry that deadlocks when a plugin consults it; the audio path stalled for the length of an inference. | `await_holding_lock` and `await_holding_refcell_ref` deny in the workspace lints; review for model runs and foreign code |
| A2 | Every await inside a session loop is bounded by a timeout or a cancellation token. A `select!` arm does not await an unbounded send. | One stalled peer freezing audio, barge-in and cancel for the whole session. | review |
| A3 | Inference and other blocking work run on the blocking pool, with their own lock, not the one the audio path takes. | Stalling an async worker thread. | review |
| A4 | A transform over a stream keeps its state across chunks. A stateless function is not called per chunk on a resampler, a framer, a decoder or a splitter. | Drift, clicks and lost samples at every chunk edge. | chunk-size-independence tests (see T6) |
| A5 | A function that decides on partial data is told whether more is coming. End of buffer is not end of stream. | "about 3." spoken before "14" arrives. | the SDK splitter takes `at_end`, and its test feeds every case by character and by word against the whole-text result; review for other partial-data decisions |
| A6 | A drain of queued input stops at the utterance boundary and takes only what was queued when it started. | Two utterances decoded as one; no interim results under load. | SDK conformance helpers for recognisers |
| A7 | A failure is reported as a failure. Code does not return the input, an empty value or `Ok` in place of an error, and does not record success for a cancelled or failed call. | The prompt spoken as the reply; a vendor error logged and then counted as a healthy turn. | review |
| A8 | A permanent error is never retried. Retries are bounded, back off with jitter, and honour the server's retry-after up to a stated cap; a wait past the cap fails the call instead. | Replaying a rejected request; synchronised retry storms; a `Retry-After: 3600` holding a spoken turn for an hour. | the retry tests in the SDK (`is_transient` per error type, jitter bounds), the pipeline's LLM retry and the OpenAI-compatible client; review for new retry loops |
| A9 | A log line that says something succeeded is written after the success is confirmed. | "CUDA registered" on a session running on the CPU. | review |
| A10 | A value that can change after construction is read where it is used. A change that replaces a per-call read with a value captured once (at connect, at build) checks every writer of that value first. | An agent's input rate, learnt after connecting, ignored by a resampler built at connect: 16 kHz audio sent to a 24 kHz agent. | the realtime round-trip test that changes the input rate after connecting; review |

## R. Readability and change

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| R1 | A function does one thing a reader can name. Past roughly 80 lines, or three levels of nesting inside a loop, extract the steps; a stream loop reads as a list of named steps. | Two copies of a commit block drifting apart inside one 600-line function. | `functions_stay_short_enough_to_read` (100 code lines, async functions included; today's longer ones listed until split); review |
| R2 | A comment says why, or how the mechanism works. It does not restate the code, record history ("was", "used to", "moved from"), or cite a ticket, plan or review. | Comments that are wrong the day after the next refactor. | review |
| R3 | When behaviour changes, the docs, the doc comments and the comments that describe it change in the same commit. | Module docs naming removed items. | docs gate; CI step `library docs build without broken links` (every crate but the engine, CLI and bench, warnings as errors) |
| R4 | Every `#[allow]` carries a `reason`, and the reason is still true. | Suppressions that outlive their cause. | clippy `allow_attributes_without_reason`; suppressions are `#[expect]`, so the compiler reports one the day its lint stops firing (an `#[allow]` remains only where the lint fires under some features) |
| R5 | Names say what a thing does, not who uses it. A contract does not mention a benchmark, a vendor or a caller. | Knowledge of one consumer baked into a shared type. | review |
| R6 | One source of truth for a list. Features, stages, provider names and defaults are defined once and read everywhere else. | Seven hand-kept copies of a feature list, one of them wrong. | workspace rules (`the_cli_defaults_are_the_batteries_and_the_listed_plugins` follows the feature graph, `every_provider_is_wired_into_the_binary`, `the_dockerfiles_default_to_the_image_feature_lists` over the one compose list); `plugins::Stage` for stages; reference generator where it exists; review |
| R7 | Dead code is removed, not kept "for later". An option nothing reads, a method nothing calls, a test that cannot fail. | Settings that look live and do nothing. | the compiler's `dead_code` lint; review for options and schema entries |
| R8 | A fix covers the class, not the instance. Before closing a bug, search for its siblings: the function's other variants (`resample` and `resample_f32`), the same code in the other provider crates, and a crate's own copy of a shared helper. Fix them in the same change or name them in its message. | A zero-rate check added to two resamplers and not the third; "no audio is an error" in one TTS crate of four; the prompt-as-reply bug fixed in one LLM crate and left in the other. | review |

## T. Tests

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| T1 | A test never passes by not running. A test that needs a model file or a paid API is `#[ignore = "…"]`; asked for without its prerequisite, it fails and names what is missing. Feature-gated tests use `cfg_attr(…, ignore)` rather than compiling out. | Suites that reported ok for months while testing nothing. | CI step `no test passed by skipping` (fails on "skipping" in the output of the `workspace tests` job: engine, library crates, bench and CLI; the `bare-engine` and `image-features` jobs' test output is not scanned); review for new early returns |
| T2 | A fix lands with a test that fails without it. | The same bug returning. | review |
| T3 | A test is deterministic: no real environment variables (inject the lookup), no fixed sleeps (paused clock, or poll with a deadline), no fixed ports, no assertion on how long something took. | Flakes that are dismissed until they are real. | review |
| T4 | A test does not change process-wide state others share: working directory, environment, or a global registry entry another test reads. Register under a name unique to the test. | Tests that pass only in a certain order. | `scripts/each-test-alone.py` over the wizard tests in CI; review |
| T5 | A test asserts on values and error kinds, not on substrings of messages. | Gates that switch off when a message is reworded. | `ConfigErrorKind` on every validation error; `shipped_configs_resolve_with_their_plugins` reads each file as its schema and matches on kinds; review |
| T6 | Code that consumes a stream is tested at every chunk size, including one byte or one sample at a time, and at an utterance boundary with the next one already queued. | Boundary bugs that only appear under load. | `voice_pipes_sdk::testing::chunking_invariant` (the framer, WAV stripper, resampler, sentence splitter, line reader and SSE reader tests); recogniser conformance helpers |
| T7 | A numeric front end (mel features, resampling) has a fixture with expected values and a stated tolerance. | Silent drift when shared code changes. | fixture tests in the crates that have them; review |
| T8 | Every rule check has a negative test: a broken input that the check reports. | A gate that cannot fail. | every workspace rule is a pure function with a `the_*_rule_catches_*` test; review |
| T9 | Fixtures are written with escapes (`\0`, `\r\n`), not raw control bytes. | Test files git treats as binary and nobody can review. | review |
| T10 | A test that matters runs in CI. Adding a test target means adding it to a job, or the job uses a form that picks it up. | An acceptance test nobody ever ran. | CI runs the engine as `--lib --tests` and every other library crate as `--workspace --exclude`, so a new target or crate is picked up; review for `#[ignore]`d suites |
| T11 | A claim about what a dependency does is checked by running it, not by reading its source. A capability a type declares (`continues_batches()`, "runs on CUDA") has a test that exercises it, so the declaration cannot drift from the behaviour. | A model said to continue a batch at an offset, decided by reading candle's mask code; every turn failed. | `each_architecture_says_truly_whether_it_continues_a_batch` (tiny models written in memory); review |
| T12 | A property test (streamed equals whole, every chunk size gives one result, a round trip) comes with cases that assert the expected output. A transform that is wrong the same way every time passes any property. | Initials split into four chunks and abbreviations matched inside words, whole and streamed alike, under a test that only compared the two. | review |
| T13 | Code that handles text is tested with text that is not ASCII: whitespace wider than a byte (no-break, em, ideographic spaces), CJK, accented letters. | A splitter that panicked on the first reply with a no-break space. | the sentence splitter's multi-byte whitespace test; review |

## D. Delivery

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| D1 | What ships is what was tested. The features of a published image or binary come from the same source the tests read. | An image missing the providers the wizard offers. | workspace rule `the_dockerfiles_default_to_the_image_feature_lists`; CI job `image` (builds the CPU image, compares its providers with the tested build) |
| D2 | Every supported build shape is built on a pull request: bare engine, defaults, image features, each optional feature, the declared minimum Rust version. | A feature that has not compiled since the last refactor. | CI jobs `bare-engine`, `image-features`, `image` (CPU Dockerfile), `msrv` and `release-targets` (Windows, macOS); review for the CUDA Dockerfile |
| D3 | A pure move is its own commit and keeps history; the commit that makes it build follows immediately and the pair is pushed together. No commit on a shared branch is knowingly broken without saying so in its message. | Unusable history and bisects. | review |
| D4 | A measured claim in a commit or plan comes from a command someone can re-run. | Numbers nobody can check. | `scripts/p5-measure.py`, `scripts/dep-budget.py` |
| D5 | A command's stdout carries only its output (a transcript, a `--format json` document); logs and diagnostics go to stderr. | `validate-config --format json` that no parser could read, and a CI job comparing `inspect` output that one new log line would break. | CLI smoke test `json_output_is_the_only_thing_on_stdout` |

## M. Metrics

| Id | Rule | Prevents | Held by |
|---|---|---|---|
| M1 | One definition per metric name — start anchor, end anchor, and what is inside it — in `docs/reference/metrics-glossary.md`, the same for every provider. | Providers reporting different spans under the same name. | canonical descriptor registry `voice-pipes/src/pipeline/metrics.rs`; `scripts/gen-reference.py --check` |
| M2 | Critical-path latency is measured by the engine at the trait seam with one clock (`committed_at: Instant` on `Utterance`) across all providers. A provider-reported number is a breakdown that must fit inside that span, never a replacement for it. | Provider wall clocks drifting from the engine's timeline; downstream stages getting blamed for upstream delays. | telescoping stage latency invariant tests in `orchestrate.rs` and `pipeline_integration.rs` |
| M3 | Missing is `null`, never `0`. An impossible value is a measurement fault the report flags as such (`measurement.*`), not slowness charged to the next component. | A 0 TTFT being accepted as valid and shifting the entire turn's wait onto TTS. | report anomaly detection (`measurement.zero_with_work`, `measurement.breakdown_exceeds_stage`, `measurement.stages_disagree`) |
| M4 | A new provider or metric ships with a conformance test whose deliberately slow fake proves the delay shows up in the metric. | A metric that reports static or mocked constants in production. | `voice_pipes_sdk::testing` conformance harness |
| M5 | Every provider crate runs the metrics conformance check helper. | New providers slipping in without verifying metric timing invariants. | workspace rule `every_provider_runs_the_metrics_conformance_helper` in `voice-pipes-cli/tests/workspace_rules.rs` |

---

## Before you open a pull request

A short form of the above. If an answer is "no", fix it or say why in the
pull request.

**Boundaries**
- [ ] No vendor or model name, rate or default added to the engine (B3).
- [ ] Nothing copied from another crate that should be shared (B7).
- [ ] New public items in a provider crate are needed from outside it (B5).

**Contracts**
- [ ] New SDK enums and growing structs are non-exhaustive with constructors;
      new trait methods have defaults (C2, C3).
- [ ] Every option is read with a typed reader, unknown keys are refused, and
      the schema matches what the code reads (C6, C7).

**Untrusted input**
- [ ] No `unwrap`, `expect`, indexing or `as` on external data; text cut at
      character boundaries (S6).
- [ ] Every network read is bounded in size and time; streams per read (S5).
- [ ] No payload, transcript, prompt or key in a log line or error (S2).

**Async**
- [ ] No lock across an await, a model run or foreign code (A1).
- [ ] Every await in a session loop has a timeout or is cancellable (A2).
- [ ] Failures return errors; nothing is passed through as success (A7).
- [ ] Partial data is handled as partial; stream state is kept (A4, A5).
- [ ] A value that can change later is read where it is used, not captured
      once (A10).
- [ ] Retries honour retry-after up to a cap; a new error kind is applied to
      every existing construction site (A8, C5).

**Tests**
- [ ] The change has a test that fails without it (T2).
- [ ] No test can pass without running; gated tests are `#[ignore]` (T1).
- [ ] No env reads, sleeps, fixed ports or shared global state (T3, T4).
- [ ] Stream code is tested across chunk sizes and at a boundary (T6).
- [ ] What a dependency does was checked by running it; a property test has
      expected-value cases beside it; text code is tested with non-ASCII
      input (T11, T12, T13).

**Metrics**
- [ ] Telemetry and breakdowns are measured at the seams; missing values are `None`/`null`, never 0 (M2, M3).
- [ ] Provider crate passes `voice_pipes_sdk::testing` metrics conformance (M4, M5).

**Delivery**
- [ ] The fix covers the class: siblings and copies were searched for and
      fixed or named (R8).
- [ ] Docs, doc comments and the changelog match the new behaviour (R3).
- [ ] Logs go to stderr; stdout is only the command's output (D5).
- [ ] `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`
      and the affected crates' tests pass locally.

## Changing these rules

A rule changes by pull request to this page, with the case that motivates it.
When a rule gains a check, update its "Held by" cell in the same change. When
a rule turns out to be wrong for a case, write the exception into the rule
rather than suppressing the check at the call site.
