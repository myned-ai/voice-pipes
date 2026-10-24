//! TTY prompting abstraction.
//!
//! The wizard never talks to `dialoguer` directly — it goes through [`Prompt`]
//! so tests can script answers ([`ScriptedPrompt`]) and run without a TTY.

use anyhow::Result;
use dialoguer::{Confirm, Input, Password, Select, theme::ColorfulTheme};

/// Interactive questions the wizard can ask.
pub trait Prompt {
    /// Menu selection; returns the chosen item index.
    fn select(&self, prompt: &str, items: &[&str], default: usize) -> Result<usize>;

    /// Free-text input with a default value.
    fn text(&self, prompt: &str, default: &str) -> Result<String>;

    /// Hidden input for secrets (keys must not echo to the terminal
    /// or land in shell/session history). Empty input is allowed.
    fn password(&self, prompt: &str) -> Result<String>;

    /// Yes/no confirmation.
    fn confirm(&self, prompt: &str, default: bool) -> Result<bool>;
}

/// Production implementation backed by `dialoguer`.
#[derive(Debug, Default)]
pub struct DialoguerPrompt;

impl Prompt for DialoguerPrompt {
    fn select(&self, prompt: &str, items: &[&str], default: usize) -> Result<usize> {
        Ok(Select::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .default(default)
            .items(items)
            .interact()?)
    }

    fn text(&self, prompt: &str, default: &str) -> Result<String> {
        Ok(Input::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .default(default.to_string())
            .interact_text()?)
    }

    fn password(&self, prompt: &str) -> Result<String> {
        Ok(Password::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .allow_empty_password(true)
            .interact()?)
    }

    fn confirm(&self, prompt: &str, default: bool) -> Result<bool> {
        Ok(Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .default(default)
            .interact()?)
    }
}

/// Test implementation replaying a scripted queue of answers.
///
/// Panics (with the prompt text) when an answer is missing or of the wrong
/// kind, so a drifted script fails loudly at the exact question.
#[cfg(test)]
pub mod scripted {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use anyhow::Result;

    use super::Prompt;

    /// One scripted answer. `Default` accepts whatever default the wizard
    /// offers for that question (works for `select`, `text` and `confirm`).
    #[derive(Debug, Clone)]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "general-purpose test fixture; not every variant is scripted yet"
        )
    )]
    pub enum Answer {
        Default,
        Select(usize),
        Text(String),
        Password(String),
        Confirm(bool),
    }

    #[derive(Debug)]
    pub struct ScriptedPrompt {
        answers: RefCell<VecDeque<Answer>>,
    }

    impl ScriptedPrompt {
        pub fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
            Self {
                answers: RefCell::new(answers.into_iter().collect()),
            }
        }

        fn next(&self, prompt: &str) -> Answer {
            self.answers
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| panic!("no scripted answer left for prompt: {prompt}"))
        }

        /// Asserts the wizard consumed every scripted answer.
        pub fn assert_exhausted(&self) {
            let left = self.answers.borrow();
            assert!(
                left.is_empty(),
                "unconsumed scripted answers: {:?}",
                left.iter().collect::<Vec<_>>()
            );
        }
    }

    impl Prompt for ScriptedPrompt {
        fn select(&self, prompt: &str, items: &[&str], default: usize) -> Result<usize> {
            match self.next(prompt) {
                Answer::Default => Ok(default),
                Answer::Select(idx) => {
                    assert!(
                        idx < items.len(),
                        "select index {idx} out of range for: {prompt}"
                    );
                    Ok(idx)
                }
                other => panic!("expected Select/Default for {prompt:?}, got {other:?}"),
            }
        }

        fn text(&self, prompt: &str, default: &str) -> Result<String> {
            match self.next(prompt) {
                Answer::Default => Ok(default.to_string()),
                Answer::Text(t) => Ok(t),
                other => panic!("expected Text/Default for {prompt:?}, got {other:?}"),
            }
        }

        fn password(&self, prompt: &str) -> Result<String> {
            match self.next(prompt) {
                Answer::Password(p) => Ok(p),
                other => panic!("expected Password for {prompt:?}, got {other:?}"),
            }
        }

        fn confirm(&self, prompt: &str, default: bool) -> Result<bool> {
            match self.next(prompt) {
                Answer::Default => Ok(default),
                Answer::Confirm(b) => Ok(b),
                other => panic!("expected Confirm/Default for {prompt:?}, got {other:?}"),
            }
        }
    }
}
