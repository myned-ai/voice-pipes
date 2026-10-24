//! Standard caller personas used by synthetic-caller scenarios.
//!
//! Personas are short prompts injected as the caller's system
//! `instructions` field. They are deliberately terse so cost stays
//! bounded -- every persona instructs the caller to keep replies
//! under ~15 words and to end the call after a small number of turns.
//!
//! Fixtures can either inline their own persona string or reference a
//! template by name via the `persona_template` field on
//! `SyntheticCallerConfig`; the runner resolves the template up-front
//! and passes the resolved string to [`SyntheticCaller::configure`].

/// Polite caller who greets and asks what time it is, then says goodbye.
/// Good baseline persona -- exercises a complete short conversation.
pub const POLITE_TIME: &str = "You are a polite caller. \
Greet the assistant in one short sentence and then ask what time it is. \
Keep every reply under 15 words. \
Thank the assistant and say goodbye after it answers.";

/// Impatient customer asking for a refund. Tends to interrupt -- good
/// for stressing barge-in / interruption handling.
pub const IMPATIENT_REFUND: &str = "You are an impatient customer who wants a refund right now. \
Speak in short, urgent sentences. Interrupt the agent if it stalls. \
Keep every reply under 15 words. \
End the call after the agent either approves the refund or escalates.";

/// Curious user who tests common greetings and trivia, then exits.
/// Lightweight -- good smoke persona that touches several intents.
pub const CURIOUS_GREETER: &str = "You are a curious user testing a voice assistant. \
Greet it, ask one small-talk question, then thank it and hang up. \
Keep every reply under 15 words.";

/// Persona designed to provoke barge-in: starts a new question before
/// the assistant finishes its previous reply.
pub const BARGE_IN_TESTER: &str = "You are a fast-talking caller who interrupts often. \
Ask a question, and as soon as the assistant starts replying, ask a follow-up. \
Keep every reply under 12 words. End the call after two interruptions.";

/// Resolve a template name to its prompt string. Names are matched
/// case-insensitively and use `snake_case` (matching fixture style).
/// Returns `None` for unknown names so the caller can decide whether
/// to fall back or surface an error.
pub fn lookup(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "polite_time" => Some(POLITE_TIME),
        "impatient_refund" => Some(IMPATIENT_REFUND),
        "curious_greeter" => Some(CURIOUS_GREETER),
        "barge_in_tester" => Some(BARGE_IN_TESTER),
        _ => None,
    }
}

/// All built-in template names, useful for documentation, CLI help,
/// and admin UI dropdowns.
pub const TEMPLATE_NAMES: &[&str] = &[
    "polite_time",
    "impatient_refund",
    "curious_greeter",
    "barge_in_tester",
];

#[cfg(test)]
#[path = "tests/prompts.rs"]
mod tests;
