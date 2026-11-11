use super::*;
use std::path::PathBuf;

fn cfg(model: &str, template: &str, prefill: bool, prompt: &str) -> LlmConfig {
    LlmConfig {
        model_path: PathBuf::from(model),
        tokenizer_path: None,
        system_prompt: prompt.to_string(),
        max_tokens: 128,
        temperature: 0.7,
        device: "cpu".into(),
        chat_template: template.into(),
        prefill_cache: prefill,
        repeat_penalty: 1.1,
        top_p: None,
        seed: 42,
        pool_size: 1,
    }
}

#[test]
fn the_same_model_shares_one_key() {
    assert_eq!(
        cache_key(&cfg("a.gguf", "chatml", false, "x")),
        cache_key(&cfg("a.gguf", "chatml", false, "x"))
    );
}

#[test]
fn sampling_parameters_do_not_split_the_cache() {
    // These live in atomic cells that /debug/params retunes at runtime,
    // so two sessions differing only in temperature must share one
    // loaded model rather than load the weights twice.
    let mut hot = cfg("a.gguf", "chatml", false, "x");
    hot.temperature = 1.4;
    hot.max_tokens = 999;
    hot.top_p = Some(0.9);
    hot.seed = 7;
    assert_eq!(
        cache_key(&cfg("a.gguf", "chatml", false, "x")),
        cache_key(&hot)
    );
}

#[test]
fn a_different_model_or_template_or_device_is_a_different_key() {
    let base = cache_key(&cfg("a.gguf", "chatml", false, "x"));
    assert_ne!(base, cache_key(&cfg("b.gguf", "chatml", false, "x")));
    assert_ne!(base, cache_key(&cfg("a.gguf", "gemma", false, "x")));
    let mut gpu = cfg("a.gguf", "chatml", false, "x");
    gpu.device = "cuda".into();
    assert_ne!(base, cache_key(&gpu));
}

#[test]
fn the_system_prompt_splits_the_cache_only_when_it_is_prefilled() {
    // Without prefill the prompt is assembled per turn by the memory
    // wrapper, so it does not change the loaded state. With prefill it
    // is baked into the KV cache and two prompts are two models.
    assert_eq!(
        cache_key(&cfg("a.gguf", "chatml", false, "one")),
        cache_key(&cfg("a.gguf", "chatml", false, "two"))
    );
    assert_ne!(
        cache_key(&cfg("a.gguf", "chatml", true, "one")),
        cache_key(&cfg("a.gguf", "chatml", true, "two"))
    );
}
