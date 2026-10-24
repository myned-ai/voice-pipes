//! How a plugin offers itself in the setup wizard (`voice-pipes config-wizard`).
//!
//! The wizard's own providers are described in code inside the engine; a
//! plugin cannot put code there, so it describes itself as data instead: the
//! menu label, the Cargo feature the worker image needs, how its files or
//! service are provisioned, and the settings to ask for with their defaults.
//! A plugin crate exports its entries from a `wizard_choices()` function
//! beside `factory()`; the binary registers them for the provider's stage,
//! and the wizard lists them with the built-in choices of that stage, in
//! [`WizardChoice::menu_rank`] order.
//!
//! ```rust,ignore
//! pub fn wizard_choices() -> Vec<WizardChoice> {
//!     vec![WizardChoice::new("My model (local ONNX)", WizardDeploy::LocalManual)
//!         .with_feature(Some("stt-my-model".into()))
//!         .with_fields(vec![WizardField::path("model_dir", "Model directory", "models/my-model")])
//!         .with_notes(vec!["Fetch the model with scripts/download_my_model.py.".into()])
//!         .with_menu_rank(50)]
//! }
//! ```

/// One entry a plugin adds to a wizard stage menu.
///
/// The TOML the wizard writes for it is `provider = "<factory name>"` (or the
/// stage's equivalent key) followed by one line per field, in order, all
/// under the stage's own table -- see [`WizardChoice::render`], which the
/// wizard calls and a plugin's own tests can call too.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WizardChoice {
    /// Menu text.
    pub label: String,
    /// The Cargo feature the worker image must be built with for this
    /// provider to exist, on whichever crate declares it. The repository's
    /// tests check it against the images' feature lists.
    pub feature: Option<String>,
    /// How the provider's model files or service are provisioned.
    pub deploy: WizardDeploy,
    /// Settings asked for, in order, each with its default.
    pub fields: Vec<WizardField>,
    /// Lines printed after the choice, for what the wizard cannot do itself
    /// (a download to run, a service to start).
    pub notes: Vec<String>,
    /// Preselect this entry in its stage's menu when the binary links it,
    /// instead of the menu's first entry. For the provider an operator who
    /// links it most likely wants; the engine refuses a second registered
    /// entry that claims it for the same stage. What a pipeline without the
    /// stage's section runs is separate: the binary registers that.
    pub stage_default: bool,
    /// Where this entry sits in its stage's menu: lower first. The engine's
    /// built-in choices rank 1, 2, … in their own order and come before a
    /// plugin entry of the same rank; plugin ties go by label. The order
    /// therefore does not depend on the order plugins register in. A
    /// provider that moves out of the engine keeps its place by taking the
    /// rank its built-in entry had.
    pub menu_rank: u16,
}

impl WizardChoice {
    /// The rank a choice gets unless it sets one: after any that do.
    pub const DEFAULT_MENU_RANK: u16 = 100;

    /// An entry labelled `label`, provisioned as `deploy`: no feature,
    /// fields or notes, not its stage's default, ranked
    /// [`DEFAULT_MENU_RANK`](Self::DEFAULT_MENU_RANK).
    pub fn new(label: impl Into<String>, deploy: WizardDeploy) -> Self {
        Self {
            label: label.into(),
            feature: None,
            deploy,
            fields: Vec::new(),
            notes: Vec::new(),
            stage_default: false,
            menu_rank: Self::DEFAULT_MENU_RANK,
        }
    }

    with_setters! {
        with_feature => feature: Option<String>,
        with_fields => fields: Vec<WizardField>,
        with_notes => notes: Vec<String>,
        with_stage_default => stage_default: bool,
        with_menu_rank => menu_rank: u16,
    }
}

/// How a wizard choice is provisioned. Descriptive: what the wizard asks
/// for is in [`WizardChoice::fields`] (a key to store is a
/// [`WizardField::secret`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WizardDeploy {
    /// Nothing to provision.
    None,
    /// Local files the wizard cannot fetch; the user is pointed at them by
    /// the choice's notes.
    LocalManual,
    /// A hosted service that authenticates through `key_env` at run time.
    Cloud {
        /// Environment variable the provider reads its credential from.
        key_env: String,
    },
    /// Files the wizard downloads, into the location the choice's first
    /// path field names (its default is where they go by default). An
    /// operator's `config/wizard_models.json` entry under the provider's
    /// name takes precedence, so URLs can be repointed without a rebuild.
    Download {
        /// How the path field is read.
        layout: WizardLayout,
        /// The files, by name and source.
        files: Vec<WizardDownload>,
    },
    /// Files the wizard's model registry lists under `key`, downloaded into
    /// the location the choice's first path field names. The entry's own
    /// default location is that field's default, so an operator's
    /// `config/wizard_models.json` entry moves both. For a provider whose
    /// files the registry already describes.
    Registry {
        /// The registry entry.
        key: String,
    },
    /// A Docker Compose service the launcher starts next to the worker (a
    /// model server the provider talks to over HTTP).
    DockerSidecar {
        /// Compose profile that enables the service.
        profile: String,
        /// Compose service name; the worker reaches it by this host name.
        service: String,
        /// Environment variable the service reads its model location from,
        /// filled from the choice's first path field (relative to `models/`).
        model_env: Option<String>,
        /// Whether the service needs a GPU.
        requires_gpu: bool,
        /// Directories that must exist before it starts (bind mounts).
        ensure_dirs: Vec<String>,
    },
    /// A Docker Compose model server the launcher starts on a model the
    /// wizard downloads first: a presets entry for a provider that talks to
    /// a local server over HTTP (an OpenAI-compatible LLM on a llama-server
    /// with a GGUF model, say). The model's location is written to the
    /// launcher's environment, not to the TOML, so the choice needs no path
    /// field.
    DockerSidecarModel {
        /// Compose profile that enables the service.
        profile: String,
        /// Compose service name; the worker reaches it by this host name.
        service: String,
        /// Environment variable the service reads the model's location from
        /// (relative to `models/`).
        model_env: String,
        /// Whether the service needs a GPU.
        requires_gpu: bool,
        /// Directories that must exist before it starts (bind mounts).
        ensure_dirs: Vec<String>,
        /// The model it serves.
        model: WizardModel,
    },
}

/// A model a [`WizardDeploy::DockerSidecarModel`] choice downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WizardModel {
    /// Its name in the wizard's model registry. An operator's
    /// `config/wizard_models.json` entry under this name takes precedence,
    /// so URLs can be repointed without a rebuild; presets that share a model
    /// share its download.
    pub key: String,
    /// Where it goes by default: the primary file for
    /// [`WizardLayout::File`], the directory for [`WizardLayout::Dir`].
    pub default_path: String,
    /// How `default_path` is read.
    pub layout: WizardLayout,
    /// The files, by name and source.
    pub files: Vec<WizardDownload>,
}

impl WizardModel {
    /// The model `key`, at `default_path` read as `layout`, made of `files`.
    pub fn new(
        key: impl Into<String>,
        default_path: impl Into<String>,
        layout: WizardLayout,
        files: Vec<WizardDownload>,
    ) -> Self {
        Self {
            key: key.into(),
            default_path: default_path.into(),
            layout,
            files,
        }
    }
}

/// How a download choice's path field is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WizardLayout {
    /// A directory; every file goes inside it.
    Dir,
    /// The primary file (the one named like the path); the others go next to
    /// it. A path renamed away from the primary file's name means the user
    /// brings their own model, and nothing is downloaded.
    File,
}

/// One file a download choice fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WizardDownload {
    /// File name on disk.
    pub name: String,
    /// Where to fetch it from.
    pub url: String,
    /// Other names that, when present, mean the file is already there (a
    /// quantised variant, say).
    pub alt_names: Vec<String>,
}

impl WizardDownload {
    /// A file fetched from `url` into `name`.
    pub fn new(name: &str, url: &str) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            alt_names: Vec::new(),
        }
    }

    with_setters! {
        with_alt_names => alt_names: Vec<String>,
    }
}

/// One setting a wizard choice asks for, or writes without asking.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WizardField {
    /// TOML key it is written under (for a secret, the environment variable
    /// it is stored as).
    pub key: String,
    /// Prompt text.
    pub prompt: String,
    /// Default answer (for a fixed field, the value written).
    pub default: String,
    /// What the answer is.
    pub kind: WizardFieldKind,
    /// A blank answer writes no line at all, so the provider's own default
    /// applies.
    pub optional: bool,
    /// A blank answer is an error: the provider cannot run without it.
    pub required: bool,
}

/// What a wizard field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WizardFieldKind {
    /// Free text.
    Text,
    /// A path to the provider's model files; the wizard treats it as the
    /// choice's model location.
    Path,
    /// Comma-separated answers, written as a TOML array of strings.
    List,
    /// Written as its default without asking.
    Fixed,
    /// Asked for without echo and stored in `.env` under `key`; nothing is
    /// written to the TOML.
    Secret,
    /// Written as its default without asking, verbatim: a TOML value (a
    /// number, a boolean) rather than a string.
    FixedLiteral,
    /// A `#` comment line, its text in `default`; nothing is asked.
    Comment,
}

impl WizardField {
    fn new(key: &str, prompt: &str, default: &str, kind: WizardFieldKind) -> Self {
        Self {
            key: key.into(),
            prompt: prompt.into(),
            default: default.into(),
            kind,
            optional: false,
            required: false,
        }
    }

    /// A free-text field.
    pub fn text(key: &str, prompt: &str, default: &str) -> Self {
        Self::new(key, prompt, default, WizardFieldKind::Text)
    }

    /// A model-path field.
    pub fn path(key: &str, prompt: &str, default: &str) -> Self {
        Self::new(key, prompt, default, WizardFieldKind::Path)
    }

    /// A comma-separated list, written as an array (`"en-US, hr-HR"` ->
    /// `["en-US", "hr-HR"]`).
    pub fn list(key: &str, prompt: &str, default: &str) -> Self {
        Self::new(key, prompt, default, WizardFieldKind::List)
    }

    /// A value written without asking.
    pub fn fixed(key: &str, value: &str) -> Self {
        Self::new(key, "", value, WizardFieldKind::Fixed)
    }

    /// A value written without asking and without quotes, for a setting
    /// that is a number or a boolean (`fixed_literal("max_tokens", "256")`
    /// writes `max_tokens = 256`). `literal` must be a valid TOML value; it is
    /// the plugin's own constant, never user input.
    pub fn fixed_literal(key: &str, literal: &str) -> Self {
        Self::new(key, "", literal, WizardFieldKind::FixedLiteral)
    }

    /// A credential asked for without echo and stored in `.env` as `env_key`.
    /// Leaving it blank stores nothing, and the provider reads the variable
    /// from its environment at run time.
    pub fn secret(env_key: &str, prompt: &str) -> Self {
        Self::new(env_key, prompt, "", WizardFieldKind::Secret)
    }

    /// A comment line in the written TOML, to explain the settings around
    /// it. `text` is the plugin's own; a newline in it starts a new comment
    /// line.
    pub fn comment(text: &str) -> Self {
        Self::new("", "", text, WizardFieldKind::Comment)
    }

    /// Mark the field optional: a blank answer writes nothing.
    #[must_use]
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Mark the field required: a blank answer stops the wizard with an
    /// error instead of writing a config the provider will refuse.
    #[must_use]
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

/// A rendered wizard choice: the TOML lines for its stage table, the
/// credentials to store, and the model location if it named one.
#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderedChoice {
    /// `key = value` lines, each ending in a newline.
    pub toml: String,
    /// `(environment variable, value)` pairs for `.env`, from secret fields
    /// that were answered.
    pub secrets: Vec<(String, String)>,
    /// The answer to the first path field.
    pub model_path: Option<String>,
}

impl WizardChoice {
    /// Render this choice as the wizard writes it. `provider_key` is the
    /// stage's selector (`provider`, or `type` for a text processor);
    /// `answer` is asked once per field that is not fixed, and returns what
    /// the user typed (or the default).
    pub fn render(
        &self,
        provider_key: &str,
        provider: &str,
        mut answer: impl FnMut(&WizardField) -> anyhow::Result<String>,
    ) -> anyhow::Result<RenderedChoice> {
        use std::fmt::Write;
        let mut out = RenderedChoice::default();
        let _ = writeln!(out.toml, "{provider_key} = {}", toml_string(provider));
        for field in &self.fields {
            let value = match field.kind {
                WizardFieldKind::Fixed => field.default.clone(),
                WizardFieldKind::FixedLiteral => {
                    let _ = writeln!(out.toml, "{} = {}", field.key, field.default.trim());
                    continue;
                }
                WizardFieldKind::Comment => {
                    for line in field.default.lines() {
                        let _ = writeln!(out.toml, "# {line}");
                    }
                    continue;
                }
                _ => answer(field)?,
            };
            let value = value.trim();
            if value.is_empty() && field.required {
                anyhow::bail!("{} is required", field.prompt);
            }
            if field.kind == WizardFieldKind::Secret {
                if !value.is_empty() {
                    out.secrets.push((field.key.clone(), value.to_string()));
                }
                continue;
            }
            if value.is_empty() && field.optional {
                continue;
            }
            if field.kind == WizardFieldKind::List {
                let items: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(toml_string)
                    .collect();
                let _ = writeln!(out.toml, "{} = [{}]", field.key, items.join(", "));
                continue;
            }
            if field.kind == WizardFieldKind::Path && out.model_path.is_none() {
                out.model_path = Some(value.to_string());
            }
            let _ = writeln!(out.toml, "{} = {}", field.key, toml_string(value));
        }
        Ok(out)
    }

    /// [`render`](Self::render) with every field at its default, as the
    /// wizard writes it when the user accepts every prompt.
    ///
    /// # Panics
    /// When a [`required`](WizardField::required) field's default is blank:
    /// such a choice has no default rendering, so render it with answers.
    pub fn render_defaults(&self, provider_key: &str, provider: &str) -> RenderedChoice {
        self.render(provider_key, provider, |f| Ok(f.default.clone()))
            .expect("a required field with a blank default needs an answer: use `render`")
    }
}

/// `value` as a TOML basic string: quoted, with the characters TOML does not
/// allow raw escaped, so whatever was typed lands in the file as data.
pub fn toml_string(value: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod tests;

// The typed keys are credentials; they never reach `Debug` output.
impl std::fmt::Debug for RenderedChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secrets: Vec<(&str, &str)> = self
            .secrets
            .iter()
            .map(|(key, _)| (key.as_str(), crate::secrets::REDACTED))
            .collect();
        f.debug_struct("RenderedChoice")
            .field("toml", &self.toml)
            .field("secrets", &secrets)
            .field("model_path", &self.model_path)
            .finish()
    }
}
