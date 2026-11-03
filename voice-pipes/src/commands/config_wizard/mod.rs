//! Configurator wizard to dynamically configure STT/LLM/TTS parameters.
//!
//! The wizard is data-driven: each stage's menu is a list of
//! `WizardChoice` values, the built-in ones (`providers.rs`) and then the
//! plugins'. This module only orchestrates: it shows the menus, writes the
//! chosen entry, folds its provisioning into downloads + the launcher stack
//! manifest, and writes the outputs.

mod manifest;
mod prompt;
mod providers;
pub(crate) mod registry;
#[cfg(test)]
#[path = "tests/suite.rs"]
mod tests;

use std::fs;
use std::path::Path;

use anyhow::Result;

use manifest::{LEGACY_FLAG_FILES, MANIFEST_FILE, StackManifest};
use prompt::{DialoguerPrompt, Prompt};
use providers::EmitCtx;
use registry::{
    DownloadSpec, Registry, download_file, load_model_registry, plan_missing_downloads,
};

/// Everything one wizard run decided, before any file/network side effects.
/// Pure output of [`build_plan`], so tests can assert on it directly.
#[derive(Debug)]
pub struct WizardPlan {
    pub pipeline_toml: String,
    pub worker_toml: String,
    pub manifest: StackManifest,
    pub env_to_save: Vec<(String, String)>,
    pub downloads: Vec<DownloadSpec>,
}

/// Launch interactive fleet configurator wizard to generate TOML configurations.
pub async fn run_config_wizard() -> Result<()> {
    println!("=======================================================");
    println!("          VOICE PIPES CONFIGURATION WIZARD             ");
    println!("=======================================================");
    println!("This wizard will guide you through setting up a custom");
    println!("pipeline configuration for Voice Pipes.\n");

    let prompt = DialoguerPrompt;
    let registry = load_model_registry();
    // The wizard writes next to the launcher's compose files and keys; run
    // from anywhere else it would create a tree nothing reads.
    if !Path::new("deploy/docker/docker-compose.yml").exists() {
        anyhow::bail!(
            "run the wizard from the voice-pipes/ directory (deploy/docker/docker-compose.yml not found in the current directory)"
        );
    }
    let plan = build_plan(&prompt, &registry)?;

    let Some(written) = write_outputs(&plan, Path::new("."), &prompt)? else {
        println!("\n⚠️  Kept existing configuration. Your new wizard choices were discarded.");
        println!(
            "💡 Tip: To start with existing configuration in the future without running the wizard, use: launch-stack -SkipWizard"
        );
        return Ok(());
    };

    println!("\n✅ Successfully wrote configurations:");
    println!("  - Pipeline config: {}", written.pipeline.display());
    println!("  - Worker config:   {}", written.worker.display());
    println!("  - Stack manifest:  {}", written.manifest.display());

    // Check and download missing models.
    let missing = plan_missing_downloads(&plan.downloads, &registry, Path::new(""));
    if !missing.is_empty() {
        println!("\n⚠️  Some required model files are missing for your configuration:");
        for item in &missing {
            println!("   - {}", item.dest.display());
        }

        let confirm = prompt.confirm(
            "Would you like to download these missing model files now?",
            true,
        )?;
        if confirm {
            for item in missing {
                if let Err(e) = download_file(item.url, &item.dest).await {
                    println!(
                        "❌ Failed to download model to {}: {}",
                        item.dest.display(),
                        e
                    );
                }
            }
        } else {
            println!(
                "⚠️  Skipped downloading. Please ensure the model files are present before starting the worker."
            );
        }
    }

    let from_launcher = std::env::var_os(FROM_LAUNCHER_ENV).is_some();
    if let Some(next) = next_step(from_launcher, cfg!(windows)) {
        println!("\n{next}");
    }

    Ok(())
}

/// Set by `launch-stack.ps1` / `launch-stack.sh` on the wizard they run:
/// the launcher starts the stack itself once the wizard returns.
const FROM_LAUNCHER_ENV: &str = "VP_WIZARD_FROM_LAUNCHER";

/// What to do with the files just written, or `None` when the launcher that
/// ran the wizard carries on by itself. Run by hand, the wizard is in
/// `voice-pipes/`, one level below the launchers; without `--skip-wizard`
/// the launcher would ask every question again.
fn next_step(from_launcher: bool, windows: bool) -> Option<String> {
    if from_launcher {
        return None;
    }
    let launch = if windows {
        "..\\launch-stack.ps1 -SkipWizard"
    } else {
        "../launch-stack.sh --skip-wizard"
    };
    Some(format!(
        "Next step: these files configure the Docker stack. Start it with this configuration:\n  {launch}\n\
         To run a pipeline natively instead, see examples/minimal.toml and docs/getting-started/quickstart-source.md.\n"
    ))
}

/// Paths the wizard wrote, relative to the root it was given.
#[derive(Debug)]
struct WrittenPaths {
    pipeline: std::path::PathBuf,
    worker: std::path::PathBuf,
    manifest: std::path::PathBuf,
}

/// Apply a plan under `root`: keys first (so a config never refers to a
/// key that was not stored), then the three config files. Existing config
/// files are only replaced after the user confirms; `None` means they
/// declined and nothing was written.
fn write_outputs(
    plan: &WizardPlan,
    root: &Path,
    prompt: &dyn Prompt,
) -> Result<Option<WrittenPaths>> {
    let base_dir = root.join("deploy/docker/config");
    let paths = WrittenPaths {
        pipeline: base_dir.join("pipeline-custom.toml"),
        worker: base_dir.join("worker-custom.toml"),
        manifest: base_dir.join(MANIFEST_FILE),
    };
    let existing: Vec<&Path> = [&paths.pipeline, &paths.worker, &paths.manifest]
        .into_iter()
        .filter(|p| p.exists())
        .map(std::path::PathBuf::as_path)
        .collect();
    if !existing.is_empty() {
        let names: Vec<String> = existing.iter().map(|p| p.display().to_string()).collect();
        let overwrite = prompt.confirm(
            &format!(
                "Overwrite the existing generated config ({})? (Previous files will be safely archived to deploy/docker/config/archive/)",
                names.join(", ")
            ),
            true,
        )?;
        if !overwrite {
            return Ok(None);
        }

        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let archive_dir = base_dir.join("archive").join(&timestamp);
        fs::create_dir_all(&archive_dir)?;
        for p in &existing {
            if let Some(file_name) = p.file_name() {
                let dest = archive_dir.join(file_name);
                fs::copy(p, &dest)?;
            }
        }
        let archive_disp = archive_dir.display();
        println!("\n📦 Archived previous configuration to {archive_disp}");
    }

    for (key, value) in &plan.env_to_save {
        save_env_var(root, key, value)?;
    }

    fs::create_dir_all(&base_dir)?;
    fs::write(&paths.pipeline, &plan.pipeline_toml)?;
    fs::write(&paths.worker, &plan.worker_toml)?;
    fs::write(&paths.manifest, plan.manifest.render())?;
    for legacy in LEGACY_FLAG_FILES {
        let _ = fs::remove_file(base_dir.join(legacy));
    }
    Ok(Some(paths))
}

/// Runs the interactive flow and assembles the [`WizardPlan`]. No file or
/// network side effects — everything is returned for the caller to apply.
/// The generated TOML is parsed back and validated before it is returned,
/// so a value that breaks the file (or names a provider the engine does
/// not know) is reported here rather than at worker start.
fn build_plan(prompt: &dyn Prompt, registry: &Registry) -> Result<WizardPlan> {
    let plan = assemble_plan(prompt, registry)?;
    check_plan(&plan)?;
    Ok(plan)
}

/// Parse both generated files back; a value that broke the syntax is an
/// error here rather than at worker start. Validation findings are only
/// reported: at wizard time they are usually environmental (a key not set
/// in this shell, a model not downloaded yet), and the worker image is
/// built with its own feature set, so a provider this binary does not know
/// may well exist there.
fn check_plan(plan: &WizardPlan) -> Result<()> {
    let cfg: crate::config::PipelineConfig = toml::from_str(&plan.pipeline_toml)
        .map_err(|e| anyhow::anyhow!("generated pipeline config does not parse: {e}"))?;
    let _: crate::api::types::WorkerConfig = toml::from_str(&plan.worker_toml)
        .map_err(|e| anyhow::anyhow!("generated worker config does not parse: {e}"))?;
    for e in crate::config::validate(&cfg) {
        println!("⚠️  {}", e.message);
    }
    Ok(())
}

fn assemble_plan(prompt: &dyn Prompt, registry: &Registry) -> Result<WizardPlan> {
    assemble_plan_with(prompt, registry, Menus::registered())
}

/// [`assemble_plan`] over the given stage menus. A mode whose stages this
/// build cannot fill is not offered, and with neither mode possible the
/// wizard stops before asking anything.
fn assemble_plan_with(
    prompt: &dyn Prompt,
    registry: &Registry,
    menus: Menus,
) -> Result<WizardPlan> {
    const CLASSIC: &str = "Classic Mode (STT -> LLM -> TTS)";
    const REALTIME: &str = "Speech-to-Speech Realtime Mode (one model hears and speaks)";
    let classic_gaps = menus.classic_gaps();
    let realtime_gaps = menus.realtime_gaps();
    let is_realtime = match (classic_gaps.is_empty(), realtime_gaps.is_empty()) {
        (true, true) => prompt.select("Select Pipeline Mode", &[CLASSIC, REALTIME], 0)? == 1,
        (true, false) => {
            println!("ℹ️  Classic mode only: {}", describe_gaps(&realtime_gaps));
            false
        }
        (false, true) => {
            println!(
                "ℹ️  Speech-to-speech mode only: {}",
                describe_gaps(&classic_gaps)
            );
            true
        }
        (false, false) => anyhow::bail!(
            "this build cannot configure a pipeline: {}",
            describe_gaps(
                &classic_gaps
                    .into_iter()
                    .chain(realtime_gaps)
                    .collect::<Vec<_>>()
            )
        ),
    };
    let Menus {
        stt,
        llm,
        tts,
        turn,
        realtime,
    } = menus;

    let mut ctx = EmitCtx::new(prompt, registry);
    let mut manifest = StackManifest::default();
    let mut downloads = Vec::new();

    ctx.toml
        .push_str("# Automatically generated by voice-pipes config-wizard\n\n");
    ctx.toml.push_str("[pipeline]\n");

    // Recording is an explicit choice. Captures hold raw audio and
    // transcripts of both sides; they power Session History and the
    // diagnostic report, stay on this machine, and are pruned after
    // `retention_days` (default 7). Default yes for the local stack.
    let record = prompt.confirm(
        "Record sessions to captures/ for Session History? (stores conversation audio + transcripts locally; auto-deleted after 7 days)",
        true,
    )?;
    let audit_block = if record {
        "[pipeline.audit]\nenabled = true\n\n"
    } else {
        "[pipeline.audit]\nenabled = false\n\n"
    };

    if is_realtime {
        ctx.toml.push_str("name = \"custom-realtime\"\n\n");
        ctx.toml.push_str(audit_block);

        ctx.toml.push_str("[realtime_provider]\n");
        run_stage(&mut ctx, &realtime, &mut manifest, &mut downloads)?;
        providers::emit_client_overrides(&mut ctx)?;
    } else {
        ctx.toml.push_str("name = \"custom-classic\"\n\n");
        ctx.toml.push_str(audit_block);

        // 1. STT provider
        ctx.toml.push_str("[stt]\n");
        run_stage(&mut ctx, &stt, &mut manifest, &mut downloads)?;
        ctx.toml.push('\n');

        // 2. LLM / text processor
        ctx.toml.push_str("[[text_processor]]\n");
        run_stage(&mut ctx, &llm, &mut manifest, &mut downloads)?;
        ctx.toml.push('\n');

        // 3. TTS provider
        ctx.toml.push_str("[tts]\n");
        run_stage(&mut ctx, &tts, &mut manifest, &mut downloads)?;
        ctx.toml.push('\n');

        // 4a. VAD: `auto`, with the auto backend's own settings (its model,
        // which is downloaded). Not a question: auto picks the backend and
        // falls back to energy, so there is nothing to choose between.
        write_vad(&mut ctx, &mut downloads)?;

        // 4b. Turn detection — how the pipeline decides the user has
        // *finished* speaking (consumes Silero's silence signal). See
        // src/stt/turn.rs.
        ctx.toml.push_str("[turn_detection]\n");
        run_stage(&mut ctx, &turn, &mut manifest, &mut downloads)?;
        ctx.toml.push('\n');

        ctx.toml
            .push_str("[barge_in]\nenabled = true\nmin_chunks = 3\nspeech_threshold = 200.0\n\n");
    }

    // Output chunking is a pipeline setting: the worker file has no such
    // table and would ignore it.
    ctx.toml.push_str(
        "[output]\nsample_rate = 16000\n\n[output.chunking]\nenabled = true\nframe_ms = 50\n\n",
    );
    ctx.toml
        .push_str("[transport]\nenabled = true\nbind = \"0.0.0.0\"\nport = 8800\n\n");
    // Generated configs fail closed. The launcher generates the
    // tokens into voice-pipes/.env; a loopback-only dev box can flip this.
    ctx.toml.push_str(
        "# Refuse to start unless VP_CLIENT_TOKEN and VP_INTERNAL_TOKEN are set\n\
         # (the launcher generates them into voice-pipes/.env). Set `required = false`\n\
         # only for a loopback-only dev box.\n\
         [auth]\nrequired = true\n",
    );

    // Client-side tools: definitions must live in
    // the worker config — the Playground/SDK only *fulfils* `trigger_action`,
    // it never injects definitions via `Configure.options.client_tools` — so
    // without these tables a session advertises no client tools and the model
    // can never call them. Offered in both modes; the classic and realtime
    // runners both dispatch them.
    if prompt.confirm(
        "Add demo client-side tools (navigate, play_media) for the Playground?",
        true,
    )? {
        ctx.toml.push_str(CLIENT_TOOLS_SNIPPET);
    }

    Ok(WizardPlan {
        pipeline_toml: ctx.toml,
        worker_toml: worker_toml(),
        manifest,
        env_to_save: ctx.env_to_save,
        downloads,
    })
}

/// Every wizard choice a plugin registered, with its provider name, across
/// the stages. Built-ins register none.
pub(crate) fn plugin_wizard_choices() -> Vec<(String, voice_pipes_sdk::WizardChoice)> {
    use crate::plugins::{Stage, wizard_choices};
    Stage::ALL.into_iter().flat_map(wizard_choices).collect()
}

/// Write `[vad]` as `mode = "auto"` with the auto backend's settings, each
/// at its default (a model path at the registry's, so an operator's entry
/// wins), and queue its download. With no auto backend that gives settings,
/// `auto` alone: it runs energy.
fn write_vad(ctx: &mut EmitCtx, downloads: &mut Vec<DownloadSpec>) -> Result<()> {
    use voice_pipes_sdk::{WizardDeploy, WizardFieldKind};
    let entry = crate::vad::segmenter::auto_backend()
        .and_then(|backend| crate::plugins::vad_wizard_entry(&backend));
    let Some(choice) = entry else {
        ctx.toml.push_str("[vad]\nmode = \"auto\"\n\n");
        return Ok(());
    };
    let registry_key = match &choice.deploy {
        WizardDeploy::Registry { key } => Some(key.clone()),
        _ => None,
    };
    let registry = ctx.registry;
    let rendered = choice.render("mode", "auto", |field| {
        Ok(match (field.kind, registry_key.as_deref()) {
            (WizardFieldKind::Path, Some(key)) => registry
                .get(key)
                .map_or_else(|| field.default.clone(), |c| c.default_path.clone()),
            _ => field.default.clone(),
        })
    })?;
    ctx.toml.push_str("[vad]\n");
    ctx.toml.push_str(&rendered.toml);
    ctx.toml.push('\n');
    if let (Some(key), Some(dest)) = (registry_key, rendered.model_path) {
        downloads.push(DownloadSpec {
            registry_key: key,
            dest,
        });
    }
    Ok(())
}

/// One stage's menu: its built-in and plugin entries, by menu rank.
#[derive(Debug)]
struct StageMenu {
    stage: crate::plugins::Stage,
    choices: Vec<MenuChoice>,
}

impl StageMenu {
    /// Every entry by [`WizardChoice::menu_rank`](voice_pipes_sdk::WizardChoice::menu_rank),
    /// a built-in before a plugin of the same rank, so a provider keeps its
    /// place when it moves from the engine into a plugin crate. A plugin
    /// entry for a provider a built-in already offers is dropped.
    fn new(stage: crate::plugins::Stage) -> Self {
        Self {
            stage,
            choices: merge_by_rank(
                providers::builtin_choices(stage),
                crate::plugins::wizard_choices(stage),
            ),
        }
    }

    fn is_empty(&self) -> bool {
        self.choices.is_empty()
    }
}

/// `stage`'s menu as this binary shows it, as (provider, label) in order:
/// the built-in entries and every linked plugin's, by rank.
pub fn menu(stage: crate::plugins::Stage) -> Vec<(String, String)> {
    StageMenu::new(stage)
        .choices
        .into_iter()
        .map(|c| (c.provider, c.choice.label))
        .collect()
}

/// A stage's built-in entries and its plugins' (already in rank-then-label
/// order) as one menu by rank, a built-in first among equal ranks; a plugin
/// entry for a provider a built-in offers is dropped.
fn merge_by_rank(
    builtins: Vec<(String, voice_pipes_sdk::WizardChoice)>,
    registered: Vec<(String, voice_pipes_sdk::WizardChoice)>,
) -> Vec<MenuChoice> {
    let registered: Vec<_> = registered
        .into_iter()
        .filter(|(provider, _)| !builtins.iter().any(|(b, _)| b == provider))
        .collect();
    let mut choices: Vec<MenuChoice> = builtins
        .into_iter()
        .chain(registered)
        .map(|(provider, choice)| MenuChoice { provider, choice })
        .collect();
    // A stable sort keeps the built-ins before plugins of the same rank.
    choices.sort_by_key(|c| c.choice.menu_rank);
    choices
}

/// Every stage's menu, built before the first question so a build that
/// cannot fill one is told so up front rather than after answering the
/// stages before it.
#[derive(Debug)]
struct Menus {
    stt: StageMenu,
    llm: StageMenu,
    tts: StageMenu,
    turn: StageMenu,
    realtime: StageMenu,
}

impl Menus {
    /// The menus this binary offers: the built-in entries and the ones its
    /// plugins registered.
    fn registered() -> Self {
        use crate::plugins::Stage;
        Self {
            stt: StageMenu::new(Stage::Stt),
            llm: StageMenu::new(Stage::Llm),
            tts: StageMenu::new(Stage::Tts),
            turn: StageMenu::new(Stage::Turn),
            realtime: StageMenu::new(Stage::Realtime),
        }
    }

    /// The classic mode's stages with nothing to offer.
    fn classic_gaps(&self) -> Vec<&StageMenu> {
        [&self.stt, &self.llm, &self.tts, &self.turn]
            .into_iter()
            .filter(|m| m.is_empty())
            .collect()
    }

    /// The speech-to-speech mode's stages with nothing to offer.
    fn realtime_gaps(&self) -> Vec<&StageMenu> {
        [&self.realtime]
            .into_iter()
            .filter(|m| m.is_empty())
            .collect()
    }
}

/// "nothing offered for X (build with …); …", for the stages that are empty.
fn describe_gaps(gaps: &[&StageMenu]) -> String {
    gaps.iter()
        .map(|m| {
            format!(
                "nothing offered for \"{}\" (build with {})",
                m.stage.menu_title(),
                m.stage.feature_hint()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A menu entry, with the provider name it selects.
#[derive(Debug)]
struct MenuChoice {
    provider: String,
    choice: voice_pipes_sdk::WizardChoice,
}

/// Shows a stage menu and writes the chosen provider's TOML lines.
fn run_stage(
    ctx: &mut EmitCtx,
    menu: &StageMenu,
    manifest: &mut StackManifest,
    downloads: &mut Vec<DownloadSpec>,
) -> Result<()> {
    let labels: Vec<&str> = menu
        .choices
        .iter()
        .map(|c| c.choice.label.as_str())
        .collect();
    // `assemble_plan_with` refuses a mode with an empty stage before asking
    // anything; this keeps an empty menu an error rather than a panic.
    if labels.is_empty() {
        anyhow::bail!("{}", describe_gaps(&[menu]));
    }
    // A plugin that is its stage's out-of-the-box choice is preselected
    // when the binary links it, wherever its rank lists it.
    let default = menu
        .choices
        .iter()
        .position(|c| c.choice.stage_default)
        .unwrap_or(0);
    let idx = ctx
        .prompt
        .select(menu.stage.menu_title(), &labels, default)?;
    let Some(chosen) = menu.choices.get(idx) else {
        anyhow::bail!("no menu entry {idx}");
    };
    emit_choice(ctx, menu.stage.provider_key(), chosen, manifest, downloads)
}

/// Write a choice as the SDK renders it (provider line, then one line per
/// field), store any credential it asked for, and print its notes. A choice
/// whose files the wizard downloads queues them for the path the user gave
/// (a registry entry's own location is the path's default), and one that
/// declares a sidecar has the launcher start it; any other provider's files
/// are its own to fetch.
fn emit_choice(
    ctx: &mut EmitCtx,
    provider_key: &str,
    menu_choice: &MenuChoice,
    manifest: &mut StackManifest,
    downloads: &mut Vec<DownloadSpec>,
) -> Result<()> {
    use voice_pipes_sdk::{WizardDeploy, WizardFieldKind};
    let choice = &menu_choice.choice;
    let prompt = ctx.prompt;
    let registry = ctx.registry;
    let registry_key = match &choice.deploy {
        WizardDeploy::Registry { key } => Some(key.as_str()),
        _ => None,
    };
    let rendered = choice.render(provider_key, &menu_choice.provider, |field| {
        match field.kind {
            WizardFieldKind::Secret => prompt.password(&format!(
                "{} (optional, press Enter to read {} from the environment at runtime)",
                field.prompt, field.key
            )),
            WizardFieldKind::Path => {
                let default = registry_key
                    .and_then(|key| registry.get(key))
                    .map_or_else(|| field.default.clone(), |c| c.default_path.clone());
                prompt.text(&field.prompt, &default)
            }
            _ => prompt.text(&field.prompt, &field.default),
        }
    })?;
    ctx.toml.push_str(&rendered.toml);
    for (env_key, value) in &rendered.secrets {
        ctx.save_key_if_set(env_key, value)?;
    }
    if !choice.notes.is_empty() {
        println!();
        for note in &choice.notes {
            println!("💡 {note}");
        }
    }
    match &choice.deploy {
        WizardDeploy::Download { .. } => {
            if let Some(dest) = rendered.model_path {
                downloads.push(DownloadSpec {
                    registry_key: menu_choice.provider.clone(),
                    dest,
                });
            }
        }
        WizardDeploy::Registry { key } => {
            if let Some(dest) = rendered.model_path {
                downloads.push(DownloadSpec {
                    registry_key: key.clone(),
                    dest,
                });
            }
        }
        WizardDeploy::DockerSidecar {
            profile,
            service,
            model_env,
            requires_gpu,
            ensure_dirs,
        } => record_sidecar(
            manifest,
            profile,
            service,
            model_env.as_deref(),
            *requires_gpu,
            ensure_dirs.iter().map(String::as_str),
            rendered.model_path.as_deref(),
        ),
        WizardDeploy::DockerSidecarModel {
            profile,
            service,
            model_env,
            requires_gpu,
            ensure_dirs,
            model,
        } => {
            // Where the registry puts the model (an operator's entry wins);
            // the sidecar is told, the TOML is not.
            let dest = ctx.default_path(&model.key, &model.default_path);
            record_sidecar(
                manifest,
                profile,
                service,
                Some(model_env.as_str()),
                *requires_gpu,
                ensure_dirs.iter().map(String::as_str),
                Some(&dest),
            );
            downloads.push(DownloadSpec {
                registry_key: model.key.clone(),
                dest,
            });
        }
        _ => {}
    }
    Ok(())
}

/// Record a plugin choice's Docker Compose sidecar in the stack manifest:
/// the launcher reads the manifest and starts it.
fn record_sidecar<'a>(
    manifest: &mut StackManifest,
    profile: &str,
    service: &str,
    model_env: Option<&str>,
    requires_gpu: bool,
    ensure_dirs: impl Iterator<Item = &'a str>,
    model_path: Option<&str>,
) {
    manifest.profiles.push(profile.to_string());
    manifest.services.push(service.to_string());
    if requires_gpu {
        manifest.requires_gpu.push(service.to_string());
    }
    manifest
        .ensure_dirs
        .extend(ensure_dirs.map(ToString::to_string));
    if let (Some(env), Some(path)) = (model_env, model_path) {
        // The compose sidecar mounts ../../models at /models, so the env var
        // must hold the path *relative to* the models dir (no leading
        // "models/"), else the path doubles up.
        let rel = path.strip_prefix("models/").unwrap_or(path);
        manifest.env.push((env.to_string(), rel.to_string()));
    }
    println!("\n💡 The launch script will start the `{service}` Docker sidecar automatically.");
}

/// Demo client-side tool tables appended when the user accepts the offer.
/// Mirrors the avatar examples: navigation is silent, media playback is
/// confirmed out loud (`speak_on_result`). `[[client_tool]]` tables are
/// order-free; only `client_tools_path` (not used here) must precede the
/// first table header.
const CLIENT_TOOLS_SNIPPET: &str = "\
\n# Client-side tools: executed by the connected client (Playground / SDK),\n\
# which reports back via `client_tool_result`.\n\
[[client_tool]]\n\
name = \"navigate\"\n\
description = \"Navigate the client UI to a named screen.\"\n\
speak_on_result = false\n\
[client_tool.parameters]\n\
type = \"object\"\n\
properties = { screen = { type = \"string\", description = \"Target screen name.\" } }\n\
required = [\"screen\"]\n\
\n\
[[client_tool]]\n\
name = \"play_media\"\n\
description = \"Start playback of a named media item on the client device.\"\n\
speak_on_result = true\n\
[client_tool.parameters]\n\
type = \"object\"\n\
properties = { title = { type = \"string\", description = \"Media item title or id.\" } }\n\
required = [\"title\"]\n";

/// The (static) worker config the wizard writes next to the pipeline config.
fn worker_toml() -> String {
    let mut toml = String::new();
    toml.push_str("# Generated by voice-pipes config-wizard\n\n");
    toml.push_str("id = \"worker-custom\"\n");
    toml.push_str("max_concurrent = 10\n");
    toml.push_str("debug_port = 8085\n");
    toml.push_str("debug_bind = \"0.0.0.0\"\n");
    // Compose service name: the orchestrator runs in its own container, so
    // the bind address is not reachable from it (live param edits 502
    // through the proxy otherwise).
    toml.push_str("advertise_address = \"worker-custom:8085\"\n\n");
    toml.push_str("[orchestrator]\n");
    toml.push_str("endpoint = \"http://orchestrator:9090\"\n");
    toml.push_str("heartbeat_interval_s = 5\n");
    toml.push_str("register_timeout_s = 10\n");
    toml
}

/// Set `key=val` in `<root>/.env`, replacing an existing line for the key.
/// The file holds credentials, so it is written owner-only and replaced
/// atomically (temp file plus rename): a crash mid-write cannot leave it
/// truncated with the other keys gone.
fn save_env_var(root: &Path, key: &str, val: &str) -> Result<()> {
    let env_path = root.join(".env");
    let mut current_env = String::new();
    if env_path.exists() {
        current_env = fs::read_to_string(&env_path)?;
    }

    let mut lines: Vec<String> = current_env
        .lines()
        .filter(|line| !line.trim().starts_with(&format!("{key}=")))
        .map(ToString::to_string)
        .collect();

    lines.push(format!("{key}={val}"));
    write_private(&env_path, &(lines.join("\n") + "\n"))?;
    println!("💾 Stored API key for {key} in .env");
    Ok(())
}

/// Write `contents` to `path` owner-readable only, via a sibling temp file
/// and a rename.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    drop(f);
    // An existing file keeps its old mode across the rename; bring an
    // older world-readable `.env` down to owner-only as well.
    #[cfg(unix)]
    fs::set_permissions(
        &tmp,
        <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )?;
    fs::rename(&tmp, path)?;
    Ok(())
}
