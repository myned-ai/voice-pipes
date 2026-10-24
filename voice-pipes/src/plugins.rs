//! What registering a plugin checks, and the setup wizard's entries for the
//! plugins a binary links.
//!
//! A factory is registered per stage (`stt::register_stt_factory` and its
//! siblings); a name a built-in or an earlier plugin already has is refused
//! rather than shadowed. A plugin's wizard entries are registered here,
//! against the factory they select, and listed in an order that does not
//! depend on the order the binary registered them in.

use std::sync::{Arc, OnceLock};

use voice_pipes_sdk::WizardChoice;

/// A plugin could not be registered: its name is taken, or its wizard
/// entries contradict what is already registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistrationError {
    /// `name` (the factory's name or one of its aliases) already names the
    /// `existing` provider of `stage`: one would shadow the other.
    #[error(
        "{stage} provider \"{provider}\" cannot be registered: \"{name}\" already names the {stage} provider \"{existing}\""
    )]
    NameTaken {
        stage: &'static str,
        provider: String,
        name: String,
        existing: String,
    },
    /// Wizard entries for a provider no factory of `stage` answers to.
    #[error("wizard entries for {stage} provider \"{provider}\": no such provider is registered")]
    NoSuchProvider {
        stage: &'static str,
        provider: String,
    },
    /// More than one of a stage's wizard entries claims to be its default.
    #[error("wizard {stage} stage: more than one entry claims to be the default ({})", .providers.join(", "))]
    TwoDefaults {
        stage: &'static str,
        providers: Vec<String>,
    },
    /// Wizard entries for a stage the setup wizard has no menu for (a VAD
    /// backend may give one, the settings written under `[vad]`).
    #[error("the setup wizard has no {stage} menu; it takes one entry at most")]
    NoStage { stage: &'static str },
    /// `provider` cannot be the stage's fallback: `existing` already is.
    #[error("{stage} provider \"{provider}\" cannot be the fallback: \"{existing}\" already is")]
    FallbackTaken {
        stage: &'static str,
        provider: String,
        existing: String,
    },
}

/// A plugin's factory, by the stage it serves: what [`register_plugin`]
/// registers.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Plugin {
    /// A speech-to-text provider (`[stt] provider`).
    Stt(fn() -> Box<dyn voice_pipes_sdk::SttFactory>),
    /// A text-to-speech provider (`[tts] provider`).
    Tts(fn() -> Box<dyn voice_pipes_sdk::TtsFactory>),
    /// A text processor or LLM (`[[text_processor]] type`).
    Text(fn() -> Box<dyn voice_pipes_sdk::TextProcessorFactory>),
    /// A voice activity detector (`[vad] mode`); the wizard has no stage for it.
    Vad(fn() -> Box<dyn voice_pipes_sdk::vad::VadFactory>),
    /// A turn detector (`[turn_detection] mode`).
    Turn(fn() -> Box<dyn voice_pipes_sdk::turn::TurnDetectorFactory>),
    /// A speech-to-speech provider (`[realtime_provider] provider`).
    #[cfg(feature = "realtime")]
    Realtime(fn() -> Box<dyn voice_pipes_sdk::s2s::RealtimeFactory>),
}

/// Register a plugin: its factory, then the setup wizard's `choices` for
/// it, as one step. Registering a factory and its entries separately
/// constructed the factory twice and could leave the entries out.
///
/// # Errors
/// A name or alias already taken ([`RegistrationError::NameTaken`]);
/// wizard entries that contradict those registered
/// ([`RegistrationError::TwoDefaults`]), in which case the factory stays
/// registered and none of the entries are; or more than one entry for a VAD
/// backend, which has no menu, only the `[vad]` settings it gives the
/// wizard ([`RegistrationError::NoStage`]), refused before anything is
/// registered.
pub fn register_plugin(
    plugin: Plugin,
    choices: Vec<WizardChoice>,
) -> Result<(), RegistrationError> {
    let (stage, name) = match plugin {
        Plugin::Stt(ctor) => (Stage::Stt, crate::stt::STT_FACTORIES.register_named(ctor)?),
        Plugin::Tts(ctor) => (Stage::Tts, crate::tts::TTS_FACTORIES.register_named(ctor)?),
        Plugin::Text(ctor) => (
            Stage::Llm,
            crate::text::TEXT_PROCESSOR_FACTORIES.register_named(ctor)?,
        ),
        Plugin::Turn(ctor) => (
            Stage::Turn,
            crate::stt::turn::TURN_FACTORIES.register_named(ctor)?,
        ),
        #[cfg(feature = "realtime")]
        Plugin::Realtime(ctor) => (
            Stage::Realtime,
            crate::realtime::factory::REALTIME_FACTORIES.register_named(ctor)?,
        ),
        Plugin::Vad(ctor) => {
            // No menu: one entry at most, the settings the wizard writes
            // under `[vad]` when this backend is the auto one.
            if choices.len() > 1 {
                return Err(RegistrationError::NoStage { stage: "VAD" });
            }
            let name = crate::vad::segmenter::VAD_FACTORIES.register_named(ctor)?;
            if let Some(choice) = choices.into_iter().next() {
                VAD_ENTRIES
                    .get_or_init(Default::default)
                    .lock()
                    .push((name, choice));
            }
            return Ok(());
        }
    };
    if choices.is_empty() {
        return Ok(());
    }
    register_wizard_choices(stage, &name, choices)
}

/// What a registry needs of a factory: the names it answers to.
pub(crate) trait Named {
    fn name(&self) -> &str;
    fn aliases(&self) -> &[&str];
}

macro_rules! named {
    ($($factory:path),* $(,)?) => {$(
        impl Named for dyn $factory {
            fn name(&self) -> &str {
                <dyn $factory>::name(self)
            }
            fn aliases(&self) -> &[&str] {
                <dyn $factory>::aliases(self)
            }
        }
    )*};
}

named!(
    voice_pipes_sdk::SttFactory,
    voice_pipes_sdk::TtsFactory,
    voice_pipes_sdk::TextProcessorFactory,
    voice_pipes_sdk::vad::VadFactory,
    voice_pipes_sdk::turn::TurnDetectorFactory,
    voice_pipes_sdk::s2s::RealtimeFactory,
);

/// One stage's factories: the ones built into this build, then the ones
/// plugins registered, answering to their names and aliases.
pub(crate) struct Registry<F: ?Sized + Named + 'static> {
    /// How errors name the stage.
    stage: &'static str,
    /// The built-in factories, constructed on first use.
    builtins: fn() -> Vec<Box<F>>,
    built: OnceLock<Vec<Arc<F>>>,
    plugins: Registered<F>,
}

impl<F: ?Sized + Named + 'static> Registry<F> {
    pub(crate) const fn new(stage: &'static str, builtins: fn() -> Vec<Box<F>>) -> Self {
        Self {
            stage,
            builtins,
            built: OnceLock::new(),
            plugins: Registered::new(),
        }
    }

    fn builtins(&self) -> &[Arc<F>] {
        self.built
            .get_or_init(|| (self.builtins)().into_iter().map(Arc::from).collect())
    }

    /// Every factory: built in, then registered.
    pub(crate) fn all(&self) -> Vec<Arc<F>> {
        let mut all = self.builtins().to_vec();
        all.extend(self.plugins.all());
        all
    }

    /// The factory answering to `name`, by its name or an alias.
    pub(crate) fn lookup(&self, name: &str) -> Option<Arc<F>> {
        self.all()
            .into_iter()
            .find(|f| f.name() == name || f.aliases().contains(&name))
    }

    /// Register a plugin's factory, refusing a name or alias any factory of
    /// this stage already answers to.
    pub(crate) fn register(&self, ctor: fn() -> Box<F>) -> Result<(), RegistrationError> {
        self.register_named(ctor).map(drop)
    }

    /// [`register`](Self::register), returning the name the factory
    /// registered under.
    pub(crate) fn register_named(&self, ctor: fn() -> Box<F>) -> Result<String, RegistrationError> {
        self.plugins
            .register(ctor, |new, registered| {
                let taken = self.builtins().iter().chain(registered).map(|f| {
                    (
                        f.name().to_string(),
                        f.aliases().iter().map(ToString::to_string).collect(),
                    )
                });
                check_new_name(self.stage, new.name(), new.aliases(), taken)
            })
            .map(|f| f.name().to_string())
    }
}

/// The factories plugins registered for one stage.
///
/// Each plugin's constructor runs once, when it registers, and never while
/// the list is locked: a constructor may look up the factories of its own
/// stage (to wrap or delegate to one) and sees those registered before it.
/// Listings and lookups hand out the stored factories and run no plugin
/// constructor, so such a constructor is never re-entered.
pub(crate) struct Registered<F: ?Sized + 'static> {
    factories: OnceLock<parking_lot::Mutex<Vec<Arc<F>>>>,
}

impl<F: ?Sized + 'static> Registered<F> {
    pub(crate) const fn new() -> Self {
        Self {
            factories: OnceLock::new(),
        }
    }

    /// The registered factories, in registration order.
    pub(crate) fn all(&self) -> Vec<Arc<F>> {
        self.factories
            .get()
            .map(|factories| factories.lock().clone())
            .unwrap_or_default()
    }

    /// Construct `ctor`'s factory and add it once `check` accepts it
    /// against the factories already registered here, returning it. The
    /// list only grows, so if another registration landed while `check`
    /// ran the check is made again against the longer list; two plugins
    /// can never both claim a name.
    pub(crate) fn register(
        &self,
        ctor: fn() -> Box<F>,
        check: impl Fn(&F, &[Arc<F>]) -> Result<(), RegistrationError>,
    ) -> Result<Arc<F>, RegistrationError> {
        let new: Arc<F> = Arc::from(ctor());
        loop {
            let seen = self.all();
            check(&*new, &seen)?;
            let mut factories = self.factories.get_or_init(Default::default).lock();
            if factories.len() == seen.len() {
                factories.push(Arc::clone(&new));
                return Ok(new);
            }
        }
    }
}

/// Refuse `name` (and `aliases`) when one of `taken` already answers to it.
/// `taken` yields each existing factory's name and aliases.
pub(crate) fn check_new_name(
    stage: &'static str,
    name: &str,
    aliases: &[&str],
    taken: impl IntoIterator<Item = (String, Vec<String>)>,
) -> Result<(), RegistrationError> {
    let wanted: Vec<&str> = std::iter::once(name)
        .chain(aliases.iter().copied())
        .collect();
    for (existing, existing_aliases) in taken {
        let answers = |n: &str| existing == n || existing_aliases.iter().any(|a| a == n);
        if let Some(clash) = wanted.iter().find(|n| answers(n)) {
            return Err(RegistrationError::NameTaken {
                stage,
                provider: name.to_string(),
                name: (*clash).to_string(),
                existing,
            });
        }
    }
    Ok(())
}

/// A pipeline stage a provider is chosen for: what the setup wizard
/// offers, `inspect` lists, and validation names when a provider is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Speech-to-text (`[stt] provider`).
    Stt,
    /// Text processor / LLM (`[[text_processor]] type`).
    Llm,
    /// Text-to-speech (`[tts] provider`).
    Tts,
    /// Speech-to-speech (`[realtime_provider] provider`).
    Realtime,
    /// Turn detection (`[turn_detection] mode`).
    Turn,
}

/// One provider a stage has in this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageProvider {
    pub name: String,
    pub aliases: Vec<String>,
    /// Runs in the worker, with no service to reach.
    pub is_local: bool,
}

impl StageProvider {
    fn new(name: &str, aliases: &[&str], is_local: bool) -> Self {
        Self {
            name: name.to_string(),
            aliases: aliases.iter().map(ToString::to_string).collect(),
            is_local,
        }
    }
}

impl Stage {
    /// Every stage, for code that must visit each (the CLI's coverage
    /// tests). A new stage belongs here too.
    pub const ALL: [Self; 5] = [Self::Stt, Self::Llm, Self::Tts, Self::Realtime, Self::Turn];

    /// Its short name, as `inspect` prints it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Llm => "text",
            Self::Tts => "tts",
            Self::Realtime => "realtime",
            Self::Turn => "turn",
        }
    }

    /// The config table its provider is chosen in.
    pub fn section(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Llm => "text_processor",
            Self::Tts => "tts",
            Self::Realtime => "realtime_provider",
            Self::Turn => "turn_detection",
        }
    }

    /// The key the provider is named under in that table.
    pub fn provider_key(self) -> &'static str {
        match self {
            Self::Llm => "type",
            Self::Turn => "mode",
            Self::Stt | Self::Tts | Self::Realtime => "provider",
        }
    }

    /// The setup wizard's question.
    pub fn menu_title(self) -> &'static str {
        match self {
            Self::Stt => "Choose STT Provider",
            Self::Llm => "Choose LLM/Text Processor",
            Self::Tts => "Choose TTS Provider",
            Self::Realtime => "Choose S2S Provider",
            Self::Turn => "Choose turn detection",
        }
    }

    /// What a build needs for the stage to offer something, for a build
    /// that has nothing: the hint the binary registered
    /// ([`register_build_hint`]), which knows its own features, else a
    /// plugin crate for the stage.
    pub fn feature_hint(self) -> String {
        BUILD_HINTS
            .get()
            .and_then(|h| {
                h.lock()
                    .iter()
                    .find(|(s, _)| *s == self)
                    .map(|(_, hint)| hint.clone())
            })
            .unwrap_or_else(|| format!("a plugin crate for the {} stage", self.label()))
    }

    /// The providers this build has for the stage.
    pub fn providers(self) -> Vec<StageProvider> {
        match self {
            Self::Stt => crate::stt::stt_factories()
                .iter()
                .map(|f| StageProvider::new(f.name(), f.aliases(), f.is_local()))
                .collect(),
            Self::Llm => crate::text::text_processor_factories()
                .iter()
                .map(|f| StageProvider::new(f.name(), f.aliases(), f.is_local()))
                .collect(),
            Self::Tts => crate::tts::tts_factories()
                .iter()
                .map(|f| StageProvider::new(f.name(), f.aliases(), f.is_local()))
                .collect(),
            // A speech-to-speech model is a hosted service.
            #[cfg(feature = "realtime")]
            Self::Realtime => crate::realtime::realtime_factories()
                .iter()
                .map(|f| StageProvider::new(f.name(), f.aliases(), false))
                .collect(),
            #[cfg(not(feature = "realtime"))]
            Self::Realtime => Vec::new(),
            // Every turn detector runs in the worker.
            Self::Turn => crate::stt::turn::turn_detector_factories()
                .iter()
                .map(|f| StageProvider::new(f.name(), f.aliases(), true))
                .collect(),
        }
    }

    /// The error for a provider name the stage does not have, at `field`,
    /// listing the ones it does.
    pub fn unknown_provider(
        self,
        field: impl Into<String>,
        name: &str,
    ) -> voice_pipes_sdk::ConfigError {
        let names: Vec<String> = self
            .providers()
            .into_iter()
            .map(|p| format!("\"{}\"", p.name))
            .collect();
        voice_pipes_sdk::ConfigError::new(
            field,
            format!(
                "unknown {} \"{name}\"; this build has [{}]",
                self.label(),
                names.join(", ")
            ),
        )
        .with_kind(voice_pipes_sdk::ConfigErrorKind::UnknownName)
    }

    /// How messages name it.
    pub fn label(self) -> &'static str {
        // Exhaustive, so a new stage fails to compile here until it is
        // labelled; a test holds `ALL` to every variant.
        match self {
            Self::Stt => "STT",
            Self::Llm => "text processor",
            Self::Tts => "TTS",
            Self::Realtime => "speech-to-speech",
            Self::Turn => "turn detection",
        }
    }

    /// Whether a factory named `provider` is registered for this stage.
    fn has_factory(self, provider: &str) -> bool {
        match self {
            Self::Stt => crate::stt::lookup_stt_factory(provider).is_some(),
            Self::Llm => crate::text::lookup_text_processor_factory(provider).is_some(),
            Self::Tts => crate::tts::lookup_tts_factory(provider).is_some(),
            #[cfg(feature = "realtime")]
            Self::Realtime => crate::realtime::lookup_realtime_factory(provider).is_some(),
            // A build without the speech-to-speech client has no such stage.
            #[cfg(not(feature = "realtime"))]
            Self::Realtime => false,
            Self::Turn => crate::stt::turn::lookup_turn_factory(provider).is_some(),
        }
    }
}

type Entries = parking_lot::Mutex<Vec<(Stage, String, WizardChoice)>>;

static WIZARD_ENTRIES: OnceLock<Entries> = OnceLock::new();

/// Offer `choices` in the wizard's `stage` menu, each selecting the factory
/// named `provider`. Register the factory first.
///
/// # Errors
/// When no factory named `provider` is registered for `stage`, or when a
/// choice claims to be the stage's default and another registered one
/// already does. Nothing is registered then.
pub fn register_wizard_choices(
    stage: Stage,
    provider: &str,
    choices: Vec<WizardChoice>,
) -> Result<(), RegistrationError> {
    if !stage.has_factory(provider) {
        return Err(RegistrationError::NoSuchProvider {
            stage: stage.label(),
            provider: provider.to_string(),
        });
    }
    let mut entries = WIZARD_ENTRIES.get_or_init(Default::default).lock();
    let defaults = entries
        .iter()
        .filter(|(s, _, c)| *s == stage && c.stage_default)
        .map(|(_, p, _)| p.as_str())
        .chain(choices.iter().filter(|c| c.stage_default).map(|_| provider));
    let defaults: Vec<String> = defaults.map(str::to_string).collect();
    if defaults.len() > 1 {
        return Err(RegistrationError::TwoDefaults {
            stage: stage.label(),
            providers: defaults,
        });
    }
    entries.extend(
        choices
            .into_iter()
            .map(|c| (stage, provider.to_string(), c)),
    );
    Ok(())
}

/// The plugin entries of a wizard stage, with the provider each selects,
/// ordered by [`WizardChoice::menu_rank`] and then label. The wizard merges
/// them with the stage's built-in entries by rank.
pub fn wizard_choices(stage: Stage) -> Vec<(String, WizardChoice)> {
    let mut out: Vec<(String, WizardChoice)> = WIZARD_ENTRIES
        .get()
        .map(|e| {
            e.lock()
                .iter()
                .filter(|(s, _, _)| *s == stage)
                .map(|(_, p, c)| (p.clone(), c.clone()))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|(_, a), (_, b)| {
        a.menu_rank
            .cmp(&b.menu_rank)
            .then_with(|| a.label.cmp(&b.label))
    });
    out
}

static FALLBACKS: OnceLock<parking_lot::Mutex<Vec<(Stage, String)>>> = OnceLock::new();

static BUILD_HINTS: OnceLock<parking_lot::Mutex<Vec<(Stage, String)>>> = OnceLock::new();

/// Say what a build of this binary needs for `stage` to offer something
/// ("stt-parakeet or stt-google"), for the setup wizard's message when the
/// stage has nothing. The binary knows its features; the engine does not.
/// A later hint for the stage replaces the earlier one.
pub fn register_build_hint(stage: Stage, hint: impl Into<String>) {
    let hint = hint.into();
    let mut hints = BUILD_HINTS.get_or_init(Default::default).lock();
    match hints.iter_mut().find(|(s, _)| *s == stage) {
        Some((_, existing)) => *existing = hint,
        None => hints.push((stage, hint)),
    }
}

static VAD_ENTRIES: OnceLock<parking_lot::Mutex<Vec<(String, WizardChoice)>>> = OnceLock::new();

/// The entry the VAD backend `name` registered: the settings the wizard
/// writes under `[vad]` when it is the auto backend.
pub fn vad_wizard_entry(name: &str) -> Option<WizardChoice> {
    VAD_ENTRIES.get().and_then(|e| {
        e.lock()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| c.clone())
    })
}

/// Name `provider` as what a pipeline with no section for `stage` runs.
/// The binary that links the provider decides this, as it decides what to
/// link; it is not the wizard's preselection
/// ([`WizardChoice::stage_default`]), which can rightly be a cloud service
/// that a section-less config should not quietly start calling.
///
/// # Errors
/// When no factory named `provider` is registered for `stage`
/// ([`RegistrationError::NoSuchProvider`]), or another provider is already
/// its fallback ([`RegistrationError::FallbackTaken`]). Naming the same
/// provider again is not an error.
pub fn register_fallback(stage: Stage, provider: &str) -> Result<(), RegistrationError> {
    if !stage.has_factory(provider) {
        return Err(RegistrationError::NoSuchProvider {
            stage: stage.label(),
            provider: provider.to_string(),
        });
    }
    let mut fallbacks = FALLBACKS.get_or_init(Default::default).lock();
    match fallbacks.iter().find(|(s, _)| *s == stage) {
        Some((_, existing)) if existing == provider => Ok(()),
        Some((_, existing)) => Err(RegistrationError::FallbackTaken {
            stage: stage.label(),
            provider: provider.to_string(),
            existing: existing.clone(),
        }),
        None => {
            fallbacks.push((stage, provider.to_string()));
            Ok(())
        }
    }
}

/// The provider [`register_fallback`] named for `stage`, if any.
pub fn fallback_provider(stage: Stage) -> Option<String> {
    FALLBACKS.get().and_then(|f| {
        f.lock()
            .iter()
            .find(|(s, _)| *s == stage)
            .map(|(_, p)| p.clone())
    })
}

#[cfg(test)]
#[path = "tests/plugins.rs"]
mod tests;
