//! `MockCaller` -- replays a canned transcript script with no audio.
//!
//! The script is a JSON file with the schema:
//! ```json
//! {
//!   "system": "optional persona check",
//!   "turns": [
//!     { "transcript": "Hello, can you hear me?" },
//!     { "transcript": "Tell me a joke." }
//!   ]
//! }
//! ```
//!
//! The mock caller emits each turn as a `TranscriptDelta` followed by a
//! single empty `AudioDelta` (so the runner has *something* to forward
//! to the pipeline) and a `ResponseDone`. It does not parse pipeline
//! audio -- it ignores `push_audio` calls entirely. It is sufficient to
//! exercise the runner loop, stop-condition logic, and barge-in policy
//! without external API dependencies.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;

use super::{CallerEvent, SyntheticCaller};

#[derive(Debug, Deserialize)]
struct MockScript {
    #[serde(default)]
    #[expect(
        dead_code,
        reason = "scripts may carry a system prompt; the mock replays fixed turns and never reads it"
    )]
    pub system: Option<String>,
    pub turns: Vec<MockScriptTurn>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MockScriptTurn {
    pub transcript: String,
}

#[derive(Debug)]
pub struct MockCaller {
    script: MockScript,
    queue: std::collections::VecDeque<CallerEvent>,
    cursor: usize,
}

impl MockCaller {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let script: MockScript = serde_json::from_str(&raw)?;
        Ok(Self {
            script,
            queue: std::collections::VecDeque::new(),
            cursor: 0,
        })
    }

    /// Construct a `MockCaller` from a literal script (for unit tests).
    pub fn from_turns<I>(turns: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        Self {
            script: MockScript {
                system: None,
                turns: turns
                    .into_iter()
                    .map(|t| MockScriptTurn { transcript: t })
                    .collect(),
            },
            queue: std::collections::VecDeque::new(),
            cursor: 0,
        }
    }

    fn enqueue_next_turn(&mut self) {
        if self.cursor >= self.script.turns.len() {
            return;
        }
        let turn = &self.script.turns[self.cursor];
        self.cursor += 1;
        self.queue
            .push_back(CallerEvent::TranscriptDelta(turn.transcript.clone()));
        // Empty audio frame -- enough for the runner to forward
        // *something* to the pipeline; real callers would emit PCM here.
        self.queue.push_back(CallerEvent::AudioDelta(Vec::new()));
        self.queue.push_back(CallerEvent::ResponseDone);
    }
}

#[async_trait]
impl SyntheticCaller for MockCaller {
    async fn configure(&mut self, _prompt: &str) -> anyhow::Result<()> {
        // First turn is queued lazily on the first next_event call.
        Ok(())
    }

    async fn push_audio(&mut self, _samples: &[i16]) -> anyhow::Result<()> {
        // Mock caller does not consume pipeline audio.
        Ok(())
    }

    async fn next_event(&mut self) -> Option<CallerEvent> {
        if self.queue.is_empty() {
            self.enqueue_next_turn();
        }
        self.queue.pop_front()
    }

    async fn cancel(&mut self) -> anyhow::Result<()> {
        // Drop any pending events from the current turn so the next
        // poll advances to the following scripted turn.
        self.queue.clear();
        Ok(())
    }

    async fn close(self: Box<Self>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/mock.rs"]
mod tests;
