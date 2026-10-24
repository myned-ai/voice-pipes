# Adding a model or component to the setup wizard & launch stack

**Status:** active
**Last verified:** 2026-10-01 (built-in entries are `WizardChoice` values)
**Owner:** Docs

> Audience: anyone wiring a new provider/model into `voice-pipes config-wizard`
> so it shows up in the interactive setup and launches correctly via
> `launch-stack.ps1` / `launch-stack.sh`.
>
> Field-level reference: the rustdoc in
> [`voice-pipes/src/commands/config_wizard/providers.rs`](../../voice-pipes/src/commands/config_wizard/providers.rs)
> (kept next to the code so it can't drift; this guide is the walkthrough).
>
> For *external* (out-of-tree) providers built on the SDK, see
> [PLUGIN-DEVELOPMENT-GUIDE.md](plugin-sdk.md) — this guide is
> the internal counterpart for built-in providers offered by the wizard. A
> built-in entry is the same `WizardChoice` a plugin exports (plugin guide
> §10.1); the wizard lists the built-ins first, then the plugins'.

## The one rule

Everything the wizard and launchers do is driven by **data**: the
`WizardChoice` entries `builtin_choices(stage)` returns in `providers.rs` for
the providers built into the engine, and the ones plugin crates export for
theirs (hosted providers and every speech-to-speech provider are plugins).
Adding a built-in option is **one entry** in `builtin_choices`. You never
edit:

- the wizard orchestration (`mod.rs` — `emit_choice` writes every entry, built
  in or plugin, from its fields and its `WizardDeploy`; menu indices don't
  exist outside `run_stage`),
- the download logic (`registry.rs` — generic, driven by
  `config/wizard_models.json`),
- **the launch scripts** — they parse the wizard-generated
  `deploy/docker/config/stack-manifest.env` generically. A new sidecar,
  model, or GPU requirement flows through the manifest with zero launcher
  changes.

A test suite (`config_wizard/tests/suite.rs`) turns the historical failure modes
into CI failures — see [What CI checks for you](#what-ci-checks-for-you).

## Checklist by scenario

| You are adding… | `builtin_choices` entry | wizard_models.json | docker-compose.yml | Cargo/feature work |
|---|---|---|---|---|
| A **new model** for an existing provider (e.g. another GGUF) | ✅ (or edit an existing entry's fields) | ✅ if auto-downloaded | — | — |
| A **cloud provider** (API key) | — a plugin crate: its `WizardChoice` declares `WizardDeploy::Cloud` and asks for the key with a `secret` field ([plugin guide](plugin-sdk.md)) | — | env passthrough for the key on the worker services | plugin feature in `FEATURES`, unless a CLI default |
| A **local-model provider** (ONNX/GGUF the wizard downloads) | ✅ with `WizardDeploy::Registry { key }` | ✅ new entry | — | provider feature in `FEATURES` |
| A **sidecar-served provider** (own container) | — a plugin crate: its `WizardChoice` declares `WizardDeploy::DockerSidecar`, or `DockerSidecarModel` when the wizard fetches the server's weights ([plugin guide](plugin-sdk.md)) | — (the plugin declares its files) | ✅ new service with `profiles: ["<profile>"]` | plugin feature in `FEATURES`, unless a CLI default |

Steps in detail:

1. **Runtime provider exists?** The engine-side implementation and factory
   registration come first (see the
   [plugin SDK guide](plugin-sdk.md)). The wizard only *offers* what a factory can
   build: your `provider` string must resolve to a factory of the stage,
   by name or alias.

2. **Add the entry** to the stage's arm of `builtin_choices` in
   `providers.rs`: the provider name and a `WizardChoice` whose fields are
   what the wizard asks for (a `path` field is the model location; its
   default is the registry entry's own when the deploy is
   `WizardDeploy::Registry`) and writes (`fixed`, `fixed_literal`).

3. **Registry entry** (`voice-pipes/config/wizard_models.json`) if it
   auto-downloads: `default_path`, `layout` (`"dir"` = fill a directory,
   `"file"` = primary file + siblings), `files[]` with absolute URLs.
   Optional `alt_names` on a file marks alternatives that already satisfy it
   (e.g. a quantized variant). The JSON is embedded at compile time; a
   runtime copy at `config/wizard_models.json` overrides it for URL patches.

4. **Compose wiring** (`voice-pipes/deploy/docker/docker-compose.yml`):
   - Every provider: add its cargo feature to the workers' list, the one
     `- &worker-features FEATURES=` line every worker service reuses, unless
     it's a crate default feature. The GPU worker builds the same list with
     `cuda,ort-cuda` on top (Dockerfile.cuda), which puts every linked ONNX
     provider on CUDA.
   - Cloud providers: pass the API key env var through to the worker
     services.
   - Sidecars: add the service with `profiles: ["<profile>"]`; mount
     `../../models` at `/models` if it consumes a downloaded model.

5. **Done.** Do **not** touch `launch-stack.ps1` / `launch-stack.sh`: a
   plugin's `WizardDeploy::DockerSidecar { profile, service, model_env,
   requires_gpu, ensure_dirs }` declaration is folded into `stack-manifest.env`
   (`COMPOSE_PROFILES`, `SIDECAR_SERVICES`, `REQUIRES_GPU`, `ENSURE_DIRS`,
   plus env vars like `LLAMA_MODEL`), and both launchers consume it
   generically.

## Worked example — a hypothetical local TTS

```rust
// providers.rs, in builtin_choices, the Stage::Tts arm:
(
    "acme".into(), // must name the TtsFactory (or an alias)
    local(
        "Acme TTS (Local ONNX)",
        "acme",     // the registry entry in wizard_models.json
        "tts-acme", // must be in the compose FEATURES
        WizardField::path("model", "Acme voice model path", "models/acme-voice.onnx"),
    ),
),
```

Plus: an `"acme"` entry in `config/wizard_models.json` with the model's URLs,
and `tts-acme` added to the workers' `FEATURES=` list in docker-compose.yml. That's
the whole change.

## What CI checks for you

`cargo test --lib config_wizard` (add the provider's feature flag for full
coverage):

- **Feature coverage** — your `feature` is in the workers' `FEATURES` build
  args (or crate defaults). Catches the "config parses, worker rejects
  `unknown or disabled provider`" launch failure.
- **Factory resolution** — your `provider` string resolves in the stage's
  runtime factory list. Catches typos and the name-vs-alias trap
  (`nemotron`, not `nemotron-streaming`).
- **Registry integrity** — every `WizardDeploy::Registry` key has a registry entry
  with absolute http(s) URLs.
- **Manifest round-trip** — every manifest key is consumed by both launcher
  scripts; the CLI's tests (`cargo test -p voice-pipes-cli`) check that every
  plugin sidecar's profile and service exist in docker-compose.yml and the
  launchers.
- **Config validity** — the TOML generated for your menu path parses and
  its providers/modes resolve.
- **Golden output** — add a fragment case to
  `every_choice_emits_its_golden_fragment` for your new option, pinning your
  emitted TOML.
