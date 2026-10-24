use super::*;

#[test]
fn chatml_eos_tokens() {
    let tpl = ChatMlTemplate;
    assert_eq!(tpl.eos_tokens()[0], "<|im_end|>");
}

#[test]
fn llama3_eos_tokens() {
    let tpl = Llama3Template;
    assert_eq!(tpl.eos_tokens()[0], "<|eot_id|>");
}

#[test]
fn chatml_strip_tokens() {
    let tpl = ChatMlTemplate;
    let mut text = "Hello<|im_end|> world<|im_start|>foo<|endoftext|>".to_string();
    for tok in tpl.strip_tokens() {
        text = text.replace(tok, "");
    }
    assert_eq!(text, "Hello worldfoo");
}

#[test]
fn llama3_strip_tokens() {
    let tpl = Llama3Template;
    let mut text = "Hello<|eot_id|> world<|end_of_text|>".to_string();
    for tok in tpl.strip_tokens() {
        text = text.replace(tok, "");
    }
    assert_eq!(text, "Hello world");
}

#[test]
fn from_name_chatml() {
    let tpl = from_name("chatml");
    let msg = vec![
        Message {
            role: "system".into(),
            content: "s".into(),
        },
        Message {
            role: "user".into(),
            content: "u".into(),
        },
    ];
    assert!(tpl.format_messages(&msg).contains("<|im_start|>"));
}

#[test]
fn from_name_llama3() {
    let tpl = from_name("llama3");
    let msg = vec![
        Message {
            role: "system".into(),
            content: "s".into(),
        },
        Message {
            role: "user".into(),
            content: "u".into(),
        },
    ];
    assert!(tpl.format_messages(&msg).contains("<|start_header_id|>"));
}

#[test]
fn from_name_unknown_defaults_chatml() {
    let tpl = from_name("something-new");
    let msg = vec![
        Message {
            role: "system".into(),
            content: "s".into(),
        },
        Message {
            role: "user".into(),
            content: "u".into(),
        },
    ];
    assert!(tpl.format_messages(&msg).contains("<|im_start|>"));
}

#[test]
fn gemma4_maps_assistant_to_model() {
    let tpl = Gemma4Template;
    let messages = vec![
        Message {
            role: "user".to_string(),
            content: "Hi".to_string(),
        },
        Message {
            role: "assistant".to_string(),
            content: "Hey!".to_string(),
        },
        Message {
            role: "user".to_string(),
            content: "How are you?".to_string(),
        },
    ];
    let prompt = tpl.format_messages(&messages);
    assert!(prompt.contains("<start_of_turn>model\nHey!<end_of_turn>"));
    assert!(!prompt.contains("<start_of_turn>assistant"));
}

#[test]
fn gemma4_system_prefix() {
    let tpl = Gemma4Template;
    let prefix = tpl.system_prefix("Be helpful.");
    assert!(prefix.contains("Be helpful."));
    assert!(prefix.ends_with("<end_of_turn>\n"));
    assert!(!prefix.contains("user"));
    assert!(!prefix.contains("model\n"));
}

#[test]
fn gemma4_eos_tokens() {
    let tpl = Gemma4Template;
    assert_eq!(tpl.eos_tokens()[0], "<end_of_turn>");
}

#[test]
fn gemma4_strip_tokens() {
    let tpl = Gemma4Template;
    let mut text = "Hello<end_of_turn> world<start_of_turn>foo<eos>".to_string();
    for tok in tpl.strip_tokens() {
        text = text.replace(tok, "");
    }
    assert_eq!(text, "Hello worldfoo");
}

#[test]
fn from_name_gemma4() {
    let tpl = from_name("gemma4");
    let msg = vec![
        Message {
            role: "system".into(),
            content: "s".into(),
        },
        Message {
            role: "user".into(),
            content: "u".into(),
        },
    ];
    assert!(tpl.format_messages(&msg).contains("<start_of_turn>"));
}

// ── Exact prompts, against each model's own template ─────────

fn conversation() -> Vec<Message> {
    [
        ("system", "Be helpful."),
        ("user", "Hello"),
        ("assistant", "Hi!"),
        ("user", "Bye"),
    ]
    .iter()
    .map(|(role, content)| Message {
        role: (*role).to_string(),
        content: (*content).to_string(),
    })
    .collect()
}

/// Each template renders a conversation exactly as its model's reference
/// template does, begin-of-text token included.
#[test]
fn each_template_renders_the_reference_prompt() {
    let msgs = conversation();
    assert_eq!(
        ChatMlTemplate.format_messages(&msgs),
        "<|im_start|>system\nBe helpful.<|im_end|>\n<|im_start|>user\nHello<|im_end|>\n\
         <|im_start|>assistant\nHi!<|im_end|>\n<|im_start|>user\nBye<|im_end|>\n\
         <|im_start|>assistant\n"
    );
    assert_eq!(
        Llama3Template.format_messages(&msgs),
        "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\nBe helpful.<|eot_id|>\
         <|start_header_id|>user<|end_header_id|>\n\nHello<|eot_id|>\
         <|start_header_id|>assistant<|end_header_id|>\n\nHi!<|eot_id|>\
         <|start_header_id|>user<|end_header_id|>\n\nBye<|eot_id|>\
         <|start_header_id|>assistant<|end_header_id|>\n\n"
    );
    assert_eq!(
        Gemma4Template.format_messages(&msgs),
        "<bos><start_of_turn>system\nBe helpful.<end_of_turn>\n\
         <start_of_turn>user\nHello<end_of_turn>\n<start_of_turn>model\nHi!<end_of_turn>\n\
         <start_of_turn>user\nBye<end_of_turn>\n<start_of_turn>model\n"
    );
    // Gemma 3 has no system role: the system prompt opens the first user
    // turn, as in the chat template the Gemma 3 GGUF carries.
    assert_eq!(
        Gemma3Template.format_messages(&msgs),
        "<bos><start_of_turn>user\nBe helpful.\n\nHello<end_of_turn>\n\
         <start_of_turn>model\nHi!<end_of_turn>\n\
         <start_of_turn>user\nBye<end_of_turn>\n<start_of_turn>model\n"
    );
}

/// Gemma 3 renders context the agent injected mid-conversation (a system
/// message) as a user turn, and a system prompt with no user turn yet as
/// one.
#[test]
fn gemma3_places_system_text_where_the_model_reads_it() {
    let msg = |role: &str, content: &str| Message {
        role: role.into(),
        content: content.into(),
    };
    assert_eq!(
        Gemma3Template.format_messages(&[
            msg("system", "Be brief."),
            msg("user", "Hi"),
            msg("assistant", "Hello."),
            msg("system", "The order shipped."),
        ]),
        "<bos><start_of_turn>user\nBe brief.\n\nHi<end_of_turn>\n\
         <start_of_turn>model\nHello.<end_of_turn>\n\
         <start_of_turn>user\nThe order shipped.<end_of_turn>\n<start_of_turn>model\n"
    );
    assert_eq!(
        Gemma3Template.format_messages(&[msg("system", "Be brief.")]),
        "<bos><start_of_turn>user\nBe brief.<end_of_turn>\n<start_of_turn>model\n"
    );
}

/// The system prefix is what the KV cache is primed with, so it must be
/// exactly the start of every prompt the template renders.
#[test]
fn each_system_prefix_starts_the_prompt() {
    let msgs = conversation();
    let templates: [&dyn ChatTemplate; 4] = [
        &ChatMlTemplate,
        &Llama3Template,
        &Gemma4Template,
        &Gemma3Template,
    ];
    for tpl in templates {
        let prompt = tpl.format_messages(&msgs);
        let prefix = tpl.system_prefix("Be helpful.");
        assert!(
            prompt.starts_with(&prefix),
            "{prefix:?} does not start {prompt:?}"
        );
    }
}

/// A prompt carries one begin-of-text token however the tokenizer is set
/// up: Llama 3's tokenizer adds its own when asked for special tokens, and
/// the template has already written one.
#[test]
fn a_prompt_has_one_begin_of_text_token() {
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;
    use tokenizers::processors::template::TemplateProcessing;
    let vocab = [("<unk>", 0), ("<|begin_of_text|>", 1), ("hi", 2)]
        .into_iter()
        .map(|(w, id)| (w.to_string(), id))
        .collect();
    let model = WordLevel::builder()
        .vocab(vocab)
        .unk_token("<unk>".to_string())
        .build()
        .unwrap();
    let mut tokenizer = tokenizers::Tokenizer::new(model);
    tokenizer.with_pre_tokenizer(Some(Whitespace {}));
    tokenizer.add_special_tokens(&[tokenizers::AddedToken::from("<|begin_of_text|>", true)]);
    tokenizer.with_post_processor(Some(
        TemplateProcessing::builder()
            .try_single("<|begin_of_text|> $A")
            .unwrap()
            .special_tokens(vec![("<|begin_of_text|>", 1)])
            .build()
            .unwrap(),
    ));
    let prompt = "<|begin_of_text|>hi";
    // What the processor used to do: the tokenizer's token plus the
    // template's.
    assert_eq!(tokenizer.encode(prompt, true).unwrap().get_ids(), [1, 1, 2]);
    assert_eq!(encode_prompt(&tokenizer, prompt).unwrap(), [1, 2]);
}
