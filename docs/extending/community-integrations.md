# Community integrations

**Status:** draft
**Last verified:** 2026-09-06
**Owner:** Maintainers

Providers for services we do not maintain ourselves live outside this
repository, built on the [plugin SDK](plugin-sdk.md). This page says how to
get one listed and what we expect of it. The policy borrows from projects
that have run a healthy integration ecosystem for years: core stays small,
integrations stay close to their authors.

## What belongs in core

A provider is added to this repository when it is:

- **local-first** (an in-process model that runs without a key), or
- **a major cloud vendor** for a stage we do not yet cover, and
- something the maintainers can run and test regularly.

Everything else is welcome as a community integration.

## Building one

1. Create a crate that depends on `voice-pipes-sdk` and implements the
   factory and provider traits for your stage. The
   [plugin SDK guide](plugin-sdk.md) walks through STT, TTS and LLM.
2. Register a factory before the engine starts:

   ```rust
   my_provider::register()?;   // calls voice_pipes::stt::register_stt_factory(...)
   voice_pipes::commands::run_serve(cfg, worker_config).await?;
   ```

3. Implement `as_component()` so the provider appears on `/health` and in the
   admin console, and record latency with the component core so per-turn
   metrics include it.
4. Add a `README.md` in the provider-page layout used here (what it is,
   feature, config block, env vars, capabilities, limitations, upstream) and
   at least one test that runs without credentials.
5. Add a CI badge.

A `cargo generate` template with a working TTS provider is planned; until
then the crate-level example in the SDK's rustdoc is the seed.

## Getting listed

Open a pull request that adds one row to the *Community* table below with the
crate name, the stage, a link and the SDK version it targets. We check that
the README follows the layout and that the tests run; we do not review the
provider's inference code. Rows whose repository goes silent for a year are
moved to an *Unmaintained* section.

## SDK stability

`voice-pipes-sdk` follows semantic versioning independently of the engine.
Additive fields on result structs arrive as new fields with defaults, which
is why the guide asks you to construct results with `..Default::default()`.
Breaking changes bump the minor version before 1.0 and are listed in the
changelog with a migration note.

## Community providers

| Crate | Stage | SDK | Link |
|---|---|---|---|
| — | — | — | be the first |
