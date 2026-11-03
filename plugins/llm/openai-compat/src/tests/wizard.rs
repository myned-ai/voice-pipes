use super::*;

fn by_label(fragment: &str) -> WizardChoice {
    wizard_choices()
        .into_iter()
        .find(|c| c.label.contains(fragment))
        .unwrap_or_else(|| panic!("no entry labelled like {fragment:?}"))
}

/// The server-on-host entry, every prompt accepted, writes what the engine's
/// built-in entry wrote: the golden from before the move.
#[test]
fn the_host_server_entry_writes_what_the_built_in_did() {
    let rendered = by_label("on host").render_defaults("type", "openai-compat");
    assert_eq!(
        rendered.toml,
        "type = \"openai-compat\"\n\
         base_url = \"http://host.docker.internal:8081/v1\"\n\
         model = \"gemma4-e2b\"\n\
         system_prompt = \"You are a helpful voice assistant. Be concise.\"\n\
         max_tokens = 256\n\
         temperature = 0.7\n"
    );
    assert!(rendered.secrets.is_empty());
}

/// A Gemma preset writes the sidecar's endpoint and the model it serves,
/// as the built-in did, and names no model path in the TOML: the sidecar
/// reads the downloaded file's location from its own environment.
#[test]
fn a_gemma_preset_writes_what_the_built_in_did() {
    let rendered = by_label("Gemma 3 1B").render_defaults("type", "openai-compat");
    assert_eq!(
        rendered.toml,
        "type = \"openai-compat\"\n\
         base_url = \"http://llama-server:8081/v1\"\n\
         model = \"gemma-3-1b-it\"\n\
         api_key = \"\"\n\
         system_prompt = \"You are a friendly voice assistant. Keep responses to one or two short sentences.\"\n\
         max_tokens = 1024\n\
         temperature = 1.0\n\
         top_p = 0.95\n"
    );
    assert_eq!(rendered.model_path, None);
}

/// Each preset downloads what the wizard's model registry lists under its
/// key (the operator's registry file still wins at run time), to the path
/// the built-in used, and starts the llama-server sidecar with it.
#[test]
fn each_gemma_preset_downloads_its_registry_model_for_the_sidecar() {
    let expected = [
        (
            "gemma4-e2b",
            "models/gemma4-e2b/gemma-4-E2B-it-Q4_K_M.gguf",
            "https://huggingface.co/unsloth/gemma-4-E2B-it-GGUF/resolve/main/gemma-4-E2B-it-Q4_K_M.gguf",
        ),
        (
            "gemma4-e4b",
            "models/gemma4-e4b/gemma-4-E4B-it-Q4_K_M.gguf",
            "https://huggingface.co/unsloth/gemma-4-E4B-it-GGUF/resolve/main/gemma-4-E4B-it-Q4_K_M.gguf",
        ),
        (
            "gemma4-12b",
            "models/gemma4-12b/gemma-4-12b-it-Q4_K_M.gguf",
            "https://huggingface.co/unsloth/gemma-4-12b-it-GGUF/resolve/main/gemma-4-12b-it-Q4_K_M.gguf",
        ),
        (
            "gemma3-1b",
            "models/gemma3-1b/gemma-3-1b-it-Q4_K_M.gguf",
            "https://huggingface.co/unsloth/gemma-3-1b-it-GGUF/resolve/main/gemma-3-1b-it-Q4_K_M.gguf",
        ),
        (
            "gemma3-4b",
            "models/gemma3-4b/gemma-3-4b-it-Q4_K_M.gguf",
            "https://huggingface.co/unsloth/gemma-3-4b-it-GGUF/resolve/main/gemma-3-4b-it-Q4_K_M.gguf",
        ),
    ];
    let presets: Vec<_> = wizard_choices()
        .into_iter()
        .filter_map(|c| match c.deploy {
            WizardDeploy::DockerSidecarModel {
                profile,
                service,
                model_env,
                model,
                ..
            } => Some((profile, service, model_env, model)),
            _ => None,
        })
        .collect();
    assert_eq!(presets.len(), expected.len());
    for ((profile, service, model_env, model), (key, path, url)) in presets.iter().zip(expected) {
        assert_eq!(
            (profile.as_str(), service.as_str(), model_env.as_str()),
            ("llama", "llama-server", "LLAMA_MODEL")
        );
        assert_eq!(model.key, key);
        assert_eq!(model.default_path, path);
        assert_eq!(model.layout, WizardLayout::File);
        assert_eq!(model.files.len(), 1);
        assert_eq!(model.files[0].url, url);
    }
}

/// One entry is the stage's default, labels are distinct, and the ranks put
/// the host server first and the presets in the order they are listed.
#[test]
fn one_default_distinct_labels_and_ordered_ranks() {
    let choices = wizard_choices();
    let defaults: Vec<_> = choices.iter().filter(|c| c.stage_default).collect();
    assert_eq!(defaults.len(), 1);
    assert!(defaults[0].label.contains("on host"));
    let mut labels: Vec<_> = choices.iter().map(|c| c.label.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), choices.len());
    assert!(choices.windows(2).all(|w| w[0].menu_rank < w[1].menu_rank));
    assert!(
        choices
            .iter()
            .all(|c| c.feature.as_deref() == Some(FEATURE))
    );
}

/// What each entry writes is a configuration the factory accepts.
#[test]
fn every_entry_writes_a_config_the_factory_accepts() {
    use voice_pipes_sdk::TextProcessorFactory;
    for choice in wizard_choices() {
        let toml_text = choice.render_defaults("type", "openai-compat").toml;
        let cfg: voice_pipes_sdk::TextProcessorConfig =
            toml::from_str(&toml_text).unwrap_or_else(|e| panic!("{}: {e}", choice.label));
        let errors = crate::OpenAiCompatFactory.validate(&cfg);
        assert!(errors.is_empty(), "{}: {errors:?}", choice.label);
    }
}
