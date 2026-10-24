//! Audit trail -- records pipeline events for offline inspection.

pub mod events;
pub mod retention;
pub mod session_audio;
pub mod sink;
pub mod store;
pub mod streaming;
pub mod tap;

pub use events::TraceEvent;
pub use session_audio::{SessionRecorder, Side as SessionAudioSide};
pub use sink::{AuditRecord, AuditSink};
pub use store::{
    AuditStore, InMemorySessionStore, LocalFsAuditStore, SessionFilter, SessionStore, StoredBlob,
    TurnFilter,
};
pub use streaming::{AuditEvent, StreamingAuditSink};
pub use tap::{AuditTap, TapSide};

#[cfg(test)]
pub(crate) mod test_support {
    use std::time::Duration;

    /// Poll `cond` until it holds or `timeout` elapses.
    ///
    /// Every sink in this module writes from a spawned task, so a test that
    /// records something and then asserts a file exists is waiting on another
    /// task. These tests used a fixed `sleep(100ms)`, which is a guess about
    /// how long that takes -- and a wrong one on a loaded machine, where the
    /// suite failed roughly one run in four with the writer simply not
    /// finished yet.
    ///
    /// Polling makes the wait proportional to the machine: fast when the write
    /// lands immediately, patient when it does not. The caller keeps its own
    /// assertions, so a genuine failure still reports what was actually
    /// missing rather than a timeout.
    pub(crate) async fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + timeout;
        while !cond() {
            if std::time::Instant::now() >= deadline {
                return; // let the caller's assertion produce the diagnostic
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Generous by design: it is only reached when something is wrong, and a
    /// slow CI box must not be mistaken for a broken writer.
    pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

    /// Number of finished entries directly in `dir`, or 0 if it does not
    /// exist yet. A blob being written sits in a `.tmp` sibling until its
    /// rename and is not counted.
    pub(crate) fn file_count(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |rd| {
            rd.filter_map(Result::ok)
                .filter(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
                .count()
        })
    }
}
