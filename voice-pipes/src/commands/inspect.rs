//! Inspect: print the registry of compiled-in providers and (optionally)
//! a single provider's parameter schema.
//!
//! Useful for:
//! * Discovering which providers a given binary build supports (cargo
//!   features change this at compile time).
//! * Documenting a provider's effective parameters without trawling source.
//! * Debugging "unknown provider" errors -- you can see exactly which names
//!   are registered.
//!
//! ```text
//! voice-pipes inspect                 # list all registered providers
//! voice-pipes inspect --kind tts      # list only TTS providers
//! voice-pipes inspect --provider piper
//! voice-pipes inspect --provider piper --format json
//! ```
//!
//! No config file is required -- the command operates purely on the static
//! factory registries (`stt::stt_factories()`, `tts::tts_factories()`,
//! `text::text_processor_factories()`, `vad::segmenter::vad_factories()`,
//! `stt::turn::turn_detector_factories()`, `realtime::realtime_factories()`).

use crate::api::param_schema::{ParamDescriptor, ProviderSchema};
use crate::plugins::Stage;

/// Which provider kind to inspect (`stt`, `tts`, `vad`, `text`, `turn`,
/// `realtime`, or all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectKind {
    All,
    Stt,
    Tts,
    Vad,
    Text,
    Turn,
    Realtime,
    Metrics,
}

impl InspectKind {
    pub fn parse(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "stt" => Ok(Self::Stt),
            "tts" => Ok(Self::Tts),
            "vad" => Ok(Self::Vad),
            "text" | "text_processor" => Ok(Self::Text),
            "turn" | "turn_detection" => Ok(Self::Turn),
            "realtime" | "s2s" => Ok(Self::Realtime),
            "metrics" => Ok(Self::Metrics),
            other => {
                anyhow::bail!(
                    "unknown --kind {other:?}; expected one of: all, stt, tts, vad, text, turn, realtime, metrics"
                )
            }
        }
    }
}

/// Output format for the inspect command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectFormat {
    Text,
    Json,
}

impl InspectFormat {
    pub fn parse(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => anyhow::bail!("unknown --format {other:?}; expected: text or json"),
        }
    }
}

/// One row in the registry listing.
#[derive(Debug, serde::Serialize)]
struct ProviderRow {
    kind: &'static str,
    name: String,
    aliases: Vec<String>,
    is_local: bool,
}

/// Detailed view returned for `--provider <name>`.
#[derive(Debug, serde::Serialize)]
struct ProviderDetail {
    kind: &'static str,
    name: String,
    aliases: Vec<String>,
    is_local: bool,
    descriptors: Vec<ParamDescriptor>,
    current_values: serde_json::Map<String, serde_json::Value>,
}

/// Top-level entry point for the `inspect` subcommand.
pub fn run_inspect(
    kind: InspectKind,
    provider: Option<String>,
    format: InspectFormat,
) -> Result<(), anyhow::Error> {
    if matches!(kind, InspectKind::Metrics) {
        let descriptors = crate::pipeline::metrics::all_canonical_descriptors();
        match format {
            InspectFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&descriptors)?);
            }
            InspectFormat::Text => {
                for d in &descriptors {
                    println!("=== {} ({}, kind: {}) ===", d.label, d.component_id, d.kind);
                    for f in &d.fields {
                        let start = f.start_anchor.as_deref().unwrap_or("—");
                        let end = f.end_anchor.as_deref().unwrap_or("—");
                        let owner = f.owner.as_deref().unwrap_or("—");
                        let unit = format!("{:?}", f.unit).to_lowercase();
                        let desc = f.includes.as_deref().unwrap_or(&f.label);
                        println!(
                            "  * {:<26} [{unit}] (owner: {owner}, anchors: {start} -> {end})",
                            f.key
                        );
                        println!("      {desc}");
                    }
                    println!();
                }
            }
        }
        return Ok(());
    }

    if let Some(name) = provider {
        let details = lookup_provider_details(&name, kind)?;
        match format {
            InspectFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&details)?);
            }
            InspectFormat::Text => {
                for detail in &details {
                    print_detail_text(detail);
                }
            }
        }
        return Ok(());
    }

    let rows = collect_rows(kind);
    match format {
        InspectFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        InspectFormat::Text => print_rows_text(&rows),
    }
    Ok(())
}

// ── Collectors ──────────────────────────────────────────────

impl InspectKind {
    /// Whether `stage`'s providers are listed.
    fn includes(self, stage: Stage) -> bool {
        matches!(
            (self, stage),
            (Self::All, _)
                | (Self::Stt, Stage::Stt)
                | (Self::Tts, Stage::Tts)
                | (Self::Text, Stage::Llm)
                | (Self::Turn, Stage::Turn)
                | (Self::Realtime, Stage::Realtime)
        )
    }
}

fn collect_rows(kind: InspectKind) -> Vec<ProviderRow> {
    let stage_rows = |stage: Stage| {
        stage.providers().into_iter().map(move |p| ProviderRow {
            kind: stage.key(),
            name: p.name,
            aliases: p.aliases,
            is_local: p.is_local,
        })
    };
    let mut rows = Vec::new();
    for stage in [Stage::Stt, Stage::Tts] {
        if kind.includes(stage) {
            rows.extend(stage_rows(stage));
        }
    }
    if matches!(kind, InspectKind::All | InspectKind::Vad) {
        for f in crate::vad::segmenter::vad_factories() {
            rows.push(ProviderRow {
                kind: "vad",
                name: f.name().to_string(),
                aliases: f.aliases().iter().map(ToString::to_string).collect(),
                // VAD backends are always local processes today.
                is_local: true,
            });
        }
    }
    for stage in [Stage::Llm, Stage::Turn, Stage::Realtime] {
        if kind.includes(stage) {
            rows.extend(stage_rows(stage));
        }
    }
    rows
}

fn lookup_provider_details(
    name: &str,
    kind_filter: InspectKind,
) -> Result<Vec<ProviderDetail>, anyhow::Error> {
    // A name can belong to several stages (`openai` is a TTS and a
    // speech-to-speech provider): every stage that has it is shown.
    let mut found = Vec::new();
    // Build a synthetic minimal config to feed to provider_schema(). Each
    // factory only reads fields it knows about, so a default config still
    // produces a useful descriptor list (just with unset current values).

    if matches!(kind_filter, InspectKind::All | InspectKind::Stt)
        && let Some(f) = crate::stt::lookup_stt_factory(name)
    {
        let cfg = crate::config::SttConfig::new(name.to_string());
        let schema = f.provider_schema(&cfg);
        found.push(into_detail("stt", &*f, schema));
    }

    if matches!(kind_filter, InspectKind::All | InspectKind::Tts)
        && let Some(f) = crate::tts::lookup_tts_factory(name)
    {
        let cfg = crate::config::TtsConfig::new(name.to_string());
        let schema = f.provider_schema(&cfg);
        found.push(ProviderDetail {
            kind: "tts",
            name: f.name().to_string(),
            aliases: alias_list(f.aliases()),
            is_local: f.is_local(),
            descriptors: schema.descriptors,
            current_values: schema.current_values,
        });
    }

    if matches!(kind_filter, InspectKind::All | InspectKind::Text)
        && let Some(f) = crate::text::lookup_text_processor_factory(name)
    {
        let cfg = crate::config::TextProcessorConfig::new(name.to_string());
        let schema = f.provider_schema(&cfg);
        found.push(ProviderDetail {
            kind: "text",
            name: f.name().to_string(),
            aliases: alias_list(f.aliases()),
            is_local: f.is_local(),
            descriptors: schema.descriptors,
            current_values: schema.current_values,
        });
    }

    if matches!(kind_filter, InspectKind::All | InspectKind::Vad)
        && let Some(f) = crate::vad::segmenter::vad_factories()
            .into_iter()
            .find(|f| f.matches(name))
    {
        // VadFactory doesn't expose a provider_schema today; emit just
        // the name so the row is still informative.
        let schema = ProviderSchema::fallback(f.name(), "Voice activity detection backend.");
        found.push(ProviderDetail {
            kind: "vad",
            name: f.name().to_string(),
            aliases: alias_list(f.aliases()),
            is_local: true,
            descriptors: schema.descriptors,
            current_values: schema.current_values,
        });
    }

    if matches!(kind_filter, InspectKind::All | InspectKind::Turn)
        && let Some(f) = crate::stt::turn::lookup_turn_factory(name)
    {
        found.push(ProviderDetail {
            kind: "turn",
            name: f.name().to_string(),
            aliases: alias_list(f.aliases()),
            is_local: true,
            descriptors: f.param_schema(),
            current_values: serde_json::Map::new(),
        });
    }

    #[cfg(feature = "realtime")]
    if matches!(kind_filter, InspectKind::All | InspectKind::Realtime)
        && let Some(f) = crate::realtime::lookup_realtime_factory(name)
    {
        // The engine's own `[realtime_provider]` settings; a provider's
        // extra keys live under `options`.
        let cfg: crate::config::RealtimeProviderConfig =
            toml::from_str(&format!("provider = {:?}", f.name()))?;
        found.push(ProviderDetail {
            kind: "realtime",
            name: f.name().to_string(),
            aliases: alias_list(f.aliases()),
            is_local: false,
            descriptors: crate::api::param_schema::ComponentParamSchema::param_schema(&cfg),
            current_values: serde_json::Map::new(),
        });
    }

    if found.is_empty() {
        anyhow::bail!(
            "unknown provider {name:?} (compiled-in providers: run `voice-pipes inspect` to list)"
        );
    }
    Ok(found)
}

/// A factory's other names, as the report lists them.
fn alias_list(aliases: &[&str]) -> Vec<String> {
    aliases.iter().map(ToString::to_string).collect()
}

fn into_detail(
    kind: &'static str,
    f: &dyn crate::stt::SttFactory,
    schema: ProviderSchema,
) -> ProviderDetail {
    ProviderDetail {
        kind,
        name: f.name().to_string(),
        aliases: alias_list(f.aliases()),
        is_local: f.is_local(),
        descriptors: schema.descriptors,
        current_values: schema.current_values,
    }
}

// ── Text printers ──────────────────────────────────────────

fn print_rows_text(rows: &[ProviderRow]) {
    if rows.is_empty() {
        println!("(no providers compiled in for the requested kind)");
        return;
    }

    // Group by kind for readability.
    let mut current_kind: Option<&str> = None;
    for row in rows {
        if Some(row.kind) != current_kind {
            if current_kind.is_some() {
                println!();
            }
            println!("[{}]", row.kind.to_uppercase());
            current_kind = Some(row.kind);
        }
        let locality = if row.is_local { "local" } else { "remote" };
        if row.aliases.is_empty() {
            println!("  {:<20}  {}", row.name, locality);
        } else {
            println!(
                "  {:<20}  {}  (aliases: {})",
                row.name,
                locality,
                row.aliases.join(", ")
            );
        }
    }
}

fn print_detail_text(detail: &ProviderDetail) {
    println!(
        "{} provider: {}  ({})",
        detail.kind.to_uppercase(),
        detail.name,
        if detail.is_local { "local" } else { "remote" }
    );
    if !detail.aliases.is_empty() {
        println!("  aliases: {}", detail.aliases.join(", "));
    }
    if detail.descriptors.is_empty() {
        println!("  (no parameters declared)");
        return;
    }
    println!("  parameters:");
    for d in &detail.descriptors {
        let mutable = if d.mutable { "mut" } else { "ro " };
        let value = detail
            .current_values
            .get(&d.key)
            .map_or_else(|| "null".to_string(), std::string::ToString::to_string);
        println!(
            "    {} {:<24} {:<8} = {}",
            mutable,
            d.key,
            format!("{:?}", d.value_type),
            value
        );
        if !d.description.is_empty() {
            println!("        {}", d.description);
        }
    }
}

#[cfg(test)]
#[path = "tests/inspect.rs"]
mod tests;
