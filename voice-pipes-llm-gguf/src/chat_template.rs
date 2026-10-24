//! Chat template rendering -- converts message lists to model prompts.

use std::fmt::Write;
/// A single message in a chat conversation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Chat template format for prompt construction and EOS detection.
///
/// Each template knows how to wrap messages and which special
/// tokens to look for when stopping / cleaning output.
pub trait ChatTemplate: Send + Sync {
    /// Format a history of messages into a full prompt string, beginning
    /// with the model's begin-of-text token when it has one. Prompts are
    /// encoded without the tokenizer adding special tokens
    /// ([`encode_prompt`]), so this is the only place the token comes from.
    fn format_messages(&self, messages: &[Message]) -> String;

    /// Prefix containing just the system-prompt portion of the template.
    /// Used for KV-cache prefill -- the processor can forward this once
    /// at startup and snapshot the KV state.
    fn system_prefix(&self, system: &str) -> String;

    /// Return EOS token strings in priority order.  The processor tries
    /// each one against the tokenizer and uses the first match.
    fn eos_tokens(&self) -> &[&str];

    /// Return special-token strings that should be stripped from decoded
    /// output text.
    fn strip_tokens(&self) -> &[&str];
}

// ── ChatML ──────────────────────────────────────────────────

/// `ChatML` format used by `SmolLM2`, Qwen, Mistral-Instruct, etc.
///
/// ```text
/// <|im_start|>system\n{system}<|im_end|>
/// <|im_start|>user\n{user}<|im_end|>
/// <|im_start|>assistant\n
/// ```
#[derive(Debug)]
pub struct ChatMlTemplate;

impl ChatTemplate for ChatMlTemplate {
    fn format_messages(&self, messages: &[Message]) -> String {
        let mut prompt = String::new();
        for msg in messages {
            let _ = write!(
                prompt,
                "<|im_start|>{}\n{}<|im_end|>\n",
                msg.role, msg.content
            );
        }
        prompt.push_str("<|im_start|>assistant\n");
        prompt
    }

    fn system_prefix(&self, system: &str) -> String {
        format!("<|im_start|>system\n{system}<|im_end|>\n")
    }

    fn eos_tokens(&self) -> &[&str] {
        &["<|im_end|>", "<|endoftext|>"]
    }

    fn strip_tokens(&self) -> &[&str] {
        &["<|im_end|>", "<|im_start|>", "<|endoftext|>"]
    }
}

// ── Llama 3 ─────────────────────────────────────────────────

/// Llama 3 / 3.1 / 3.2 instruct format.
///
/// ```text
/// <|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{system}<|eot_id|>
/// <|start_header_id|>user<|end_header_id|>\n\n{user}<|eot_id|>
/// <|start_header_id|>assistant<|end_header_id|>\n\n
/// ```
#[derive(Debug)]
pub struct Llama3Template;

impl ChatTemplate for Llama3Template {
    fn format_messages(&self, messages: &[Message]) -> String {
        let mut prompt = String::from("<|begin_of_text|>");
        for msg in messages {
            let role = match msg.role.as_str() {
                "system" => "system",
                "assistant" => "assistant",
                _ => "user", // fallback
            };
            let _ = write!(
                prompt,
                "<|start_header_id|>{role}<|end_header_id|>\n\n{}<|eot_id|>",
                msg.content
            );
        }
        prompt.push_str("<|start_header_id|>assistant<|end_header_id|>\n\n");
        prompt
    }

    fn system_prefix(&self, system: &str) -> String {
        format!(
            "<|begin_of_text|>\
             <|start_header_id|>system<|end_header_id|>\n\n\
             {system}<|eot_id|>"
        )
    }

    fn eos_tokens(&self) -> &[&str] {
        &["<|eot_id|>", "<|end_of_text|>", "</s>"]
    }

    fn strip_tokens(&self) -> &[&str] {
        &[
            "<|eot_id|>",
            "<|end_of_text|>",
            "<|start_header_id|>",
            "<|end_header_id|>",
            "<|begin_of_text|>",
        ]
    }
}

// ── Gemma 4 ─────────────────────────────────────────────────

/// Gemma 4 instruct format (E2B, E4B, 26B-A4B, 31B).
///
/// Gemma 4 adds native `system` role support.  Thinking mode is
/// **disabled** by default (no `<|think|>` prefix) for low-latency
/// voice-assistant use -- callers can prepend it themselves if needed.
///
/// ```text
/// <bos><start_of_turn>system
/// {system}<end_of_turn>
/// <start_of_turn>user
/// {user}<end_of_turn>
/// <start_of_turn>model
/// ```
#[derive(Debug)]
pub struct Gemma4Template;

impl ChatTemplate for Gemma4Template {
    fn format_messages(&self, messages: &[Message]) -> String {
        let mut prompt = String::from("<bos>");
        for msg in messages {
            let role = match msg.role.as_str() {
                "assistant" => "model",
                other => other, // system, user pass through
            };
            let _ = write!(
                prompt,
                "<start_of_turn>{role}\n{}<end_of_turn>\n",
                msg.content
            );
        }
        prompt.push_str("<start_of_turn>model\n");
        prompt
    }

    fn system_prefix(&self, system: &str) -> String {
        format!("<bos><start_of_turn>system\n{system}<end_of_turn>\n")
    }

    fn eos_tokens(&self) -> &[&str] {
        &["<end_of_turn>", "<eos>"]
    }

    fn strip_tokens(&self) -> &[&str] {
        &["<end_of_turn>", "<start_of_turn>", "<eos>"]
    }
}

// ── Gemma 3 ─────────────────────────────────────────────────

/// Gemma 3 instruct format. Gemma 3 has no `system` role: as in its own
/// template, a leading system prompt opens the first user turn, followed
/// by a blank line. A system message later in the history (context the
/// agent injected) becomes a user turn, the only other place the model
/// reads text from.
///
/// ```text
/// <bos><start_of_turn>user
/// {system}
///
/// {user}<end_of_turn>
/// <start_of_turn>model
/// ```
#[derive(Debug)]
pub struct Gemma3Template;

impl ChatTemplate for Gemma3Template {
    fn format_messages(&self, messages: &[Message]) -> String {
        let (system, rest) = match messages.split_first() {
            Some((first, rest)) if first.role == "system" => (Some(first.content.as_str()), rest),
            _ => (None, messages),
        };
        let mut prompt = String::from("<bos>");
        let mut pending_system = system;
        for msg in rest {
            let role = if msg.role == "assistant" {
                "model"
            } else {
                "user"
            };
            let _ = writeln!(prompt, "<start_of_turn>{role}");
            if role == "user"
                && let Some(system) = pending_system.take()
            {
                let _ = write!(prompt, "{system}\n\n");
            }
            let _ = writeln!(prompt, "{}<end_of_turn>", msg.content);
        }
        if let Some(system) = pending_system {
            // A system prompt with no user turn yet stands as one.
            let _ = write!(prompt, "<start_of_turn>user\n{system}<end_of_turn>\n");
        }
        prompt.push_str("<start_of_turn>model\n");
        prompt
    }

    fn system_prefix(&self, system: &str) -> String {
        format!("<bos><start_of_turn>user\n{system}\n\n")
    }

    fn eos_tokens(&self) -> &[&str] {
        &["<end_of_turn>", "<eos>"]
    }

    fn strip_tokens(&self) -> &[&str] {
        &["<end_of_turn>", "<start_of_turn>", "<eos>"]
    }
}

/// Encode a formatted prompt. The template has written the model's
/// begin-of-text token already, so the tokenizer adds none: a borrowed
/// `tokenizer.json` whose post-processor also adds one would otherwise
/// start every prompt with two.
///
/// # Errors
/// When the tokenizer cannot encode the text.
pub fn encode_prompt(
    tokenizer: &tokenizers::Tokenizer,
    prompt: &str,
) -> tokenizers::Result<Vec<u32>> {
    Ok(tokenizer.encode(prompt, false)?.get_ids().to_vec())
}

// ── Factory ─────────────────────────────────────────────────

/// The template names the processor takes; the options reader refuses any
/// other.
pub const NAMES: &[&str] = &["chatml", "llama3", "gemma3", "gemma4"];

/// Build a `ChatTemplate` from a config string, one of [`NAMES`]. A name
/// outside them, which the options reader refuses first, falls back to
/// `ChatML`.
pub fn from_name(name: &str) -> Box<dyn ChatTemplate> {
    match name {
        "llama3" => Box::new(Llama3Template),
        "gemma3" => Box::new(Gemma3Template),
        "gemma4" => Box::new(Gemma4Template),
        _ => Box::new(ChatMlTemplate), // "chatml" or unknown -> ChatML
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/chat_template.rs"]
mod tests;
