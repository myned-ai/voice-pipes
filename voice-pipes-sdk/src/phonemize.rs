//! espeak-ng subprocess runner for grapheme-to-phoneme conversion.
//!
//! TTS models that take phonemes (Piper, Kokoro) shell out to espeak-ng for
//! IPA; built in or plugin, they share this one runner. It needs `espeak-ng`
//! (or `espeak`) on `PATH` at run time and nothing at build time.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::tts::TtsError;

/// Maximum duration allowed for an espeak-ng subprocess run.
const PHONEMIZE_TIMEOUT: Duration = Duration::from_secs(10);

/// Validate language code: must not be empty, <= 16 chars, cannot start with '-',
/// and must only contain ascii alphanumeric characters, '-', '_', or '+'.
fn validate_language(lang: &str) -> Result<(), TtsError> {
    if lang.is_empty()
        || lang.len() > 16
        || lang.starts_with('-')
        || !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+')
    {
        return Err(TtsError::PhonemizationFailed(format!(
            "invalid phonemizer language identifier: {lang:?}"
        )));
    }
    Ok(())
}

/// Run espeak-ng (or espeak fallback) and return raw IPA output.
pub fn run_espeak(text: &str) -> Result<String, TtsError> {
    phonemize("espeak-ng", text, None).or_else(|_| phonemize("espeak", text, None))
}

/// Run espeak-ng with an explicit language code.
pub fn run_espeak_with_language(text: &str, language: &str) -> Result<String, TtsError> {
    validate_language(language)?;
    phonemize("espeak-ng", text, Some(language))
        .or_else(|_| phonemize("espeak", text, Some(language)))
}

fn kill_process(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
}

/// Invoke `program` on `text`. The text goes in on stdin rather than as an
/// argument: a sentence that starts with a dash would otherwise be parsed
/// as an option (a markdown bullet turning into an "unknown option" exit),
/// and stdin has no length limit where argv does.
fn phonemize(program: &str, text: &str, language: Option<&str>) -> Result<String, TtsError> {
    let mut cmd = Command::new(program);
    cmd.args(["--ipa", "-q", "--stdin"]);
    if let Some(lang) = language {
        cmd.args(["-v", lang]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            TtsError::PhonemizationFailed(format!("{program} not found or failed to start: {e}"))
        })?;

    // Write on a helper thread so a child that stops reading (or exits
    // early) cannot deadlock the write against a full stdout pipe.
    let mut stdin = child.stdin.take().expect("stdin was requested");
    let payload = text.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
        let _ = stdin.write_all(b"\n");
        // Dropping the handle closes the pipe, which is espeak's EOF.
    });

    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let res = child.wait_with_output();
        let _ = tx.send(res);
    });

    let output = if let Ok(res) = rx.recv_timeout(PHONEMIZE_TIMEOUT) {
        let _ = waiter.join();
        let _ = writer.join();
        res.map_err(|e| TtsError::PhonemizationFailed(format!("{program} did not finish: {e}")))?
    } else {
        kill_process(pid);
        let _ = writer.join();
        return Err(TtsError::PhonemizationFailed(format!(
            "{program} timed out after {PHONEMIZE_TIMEOUT:?}"
        )));
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TtsError::PhonemizationFailed(format!(
            "{program} exited with {}: {stderr}",
            output.status
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(stdout)
}

#[cfg(test)]
#[path = "tests/phonemize.rs"]
mod tests;
