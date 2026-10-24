//! Session store -- server-side session preservation and reconnection.
//!
//! Maintains a registry of active and recently-disconnected sessions.
//! When a client reconnects within the grace period, it can resume
//! its existing pipeline session and receive any buffered messages.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::error::TransportError;
use super::{ServerMessage, SessionConfig};

// ── Configuration ───────────────────────────────────────────

/// Configuration for the session store.
#[derive(Debug, Clone)]
pub struct SessionStoreConfig {
    /// How long to keep a disconnected session before reaping.
    pub reconnect_grace_period: Duration,
    /// Maximum number of messages to buffer during disconnect.
    pub replay_buffer_size: usize,
    /// Maximum total sessions (active + disconnected).
    pub max_sessions: usize,
    /// How often the reaper task runs.
    pub reap_interval: Duration,
}

/// Default grace period: 30 seconds.
const DEFAULT_GRACE_PERIOD_SECS: u64 = 30;

/// Default replay buffer size: 100 messages.
const DEFAULT_REPLAY_BUFFER_SIZE: usize = 100;

/// Default maximum sessions.
const DEFAULT_MAX_SESSIONS: usize = 1000;

/// Default reap interval: 10 seconds.
const DEFAULT_REAP_INTERVAL_SECS: u64 = 10;

impl Default for SessionStoreConfig {
    fn default() -> Self {
        Self {
            reconnect_grace_period: Duration::from_secs(DEFAULT_GRACE_PERIOD_SECS),
            replay_buffer_size: DEFAULT_REPLAY_BUFFER_SIZE,
            max_sessions: DEFAULT_MAX_SESSIONS,
            reap_interval: Duration::from_secs(DEFAULT_REAP_INTERVAL_SECS),
        }
    }
}

// ── Session slot ────────────────────────────────────────────

/// Channels that connect a transport session to its pipeline.
#[derive(Debug)]
pub struct PipelineChannels {
    /// Send audio/text/data towards the pipeline.
    pub to_pipeline_tx: mpsc::Sender<super::ClientMessage>,
    /// Receive audio/text/data from the pipeline.
    pub from_pipeline_rx: mpsc::Receiver<ServerMessage>,
}

/// A single session slot in the store.
#[derive(Debug)]
pub struct SessionSlot {
    /// Unique session identifier.
    pub session_id: String,
    /// Opaque token for reconnection.
    pub session_token: String,
    /// Negotiated session configuration.
    pub config: SessionConfig,
    /// When this session was first created.
    pub created_at: Instant,
    /// When a transport was last connected.
    pub last_connected_at: Instant,
    /// Current connection state.
    pub state: SessionState,
    /// Messages buffered while disconnected (bounded).
    pub replay_buffer: VecDeque<ServerMessage>,
    /// Maximum replay buffer size.
    pub replay_buffer_max: usize,
}

/// Connection state of a session.
#[derive(Debug)]
pub enum SessionState {
    /// Transport is actively connected.
    Connected,
    /// Transport disconnected; waiting for reconnection.
    Disconnected {
        /// When the disconnect occurred.
        since: Instant,
    },
}

impl SessionSlot {
    /// Whether this session has expired past the grace period.
    pub fn is_expired(&self, grace_period: Duration) -> bool {
        match &self.state {
            SessionState::Connected => false,
            SessionState::Disconnected { since } => since.elapsed() > grace_period,
        }
    }

    /// Buffer a message for replay on reconnection.
    /// Drops the oldest message if the buffer is full.
    pub fn buffer_message(&mut self, msg: ServerMessage) {
        if self.replay_buffer.len() >= self.replay_buffer_max {
            self.replay_buffer.pop_front();
        }
        self.replay_buffer.push_back(msg);
    }

    /// Drain all buffered messages for replay.
    pub fn drain_replay_buffer(&mut self) -> Vec<ServerMessage> {
        self.replay_buffer.drain(..).collect()
    }
}

// ── Session store ───────────────────────────────────────────

/// Server-side session store for preservation and reconnection.
///
/// Thread-safe -- uses [`DashMap`] for concurrent access.
#[derive(Debug)]
pub struct SessionStore {
    config: SessionStoreConfig,
    /// Sessions keyed by `session_id`.
    sessions: Arc<DashMap<String, SessionSlot>>,
    /// Index from `session_token` -> `session_id` for fast reconnection lookup.
    token_index: Arc<DashMap<String, String>>,
}

impl SessionStore {
    /// Create a new session store.
    pub fn new(config: SessionStoreConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(DashMap::new()),
            token_index: Arc::new(DashMap::new()),
        }
    }

    /// Create a new session, returning its ID and token.
    ///
    /// Returns `Err` if the store has reached capacity.
    pub fn create_session(
        &self,
        config: SessionConfig,
    ) -> Result<(String, String), TransportError> {
        if self.sessions.len() >= self.config.max_sessions {
            // Make room before giving up: first reclaim grace-expired sessions,
            // then evict the least-recently-connected *disconnected* session.
            // Connected sessions are never evicted, so capacity is only reported
            // exhausted when the store is genuinely full of live sessions --
            // rather than the previous behaviour where finished-but-never-reaped
            // sessions bricked the worker after `max_sessions` handshakes.
            self.reap_expired();
            if self.sessions.len() >= self.config.max_sessions {
                self.evict_oldest_disconnected();
            }
            if self.sessions.len() >= self.config.max_sessions {
                return Err(TransportError::CapacityExhausted);
            }
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let session_token = generate_token();

        let slot = SessionSlot {
            session_id: session_id.clone(),
            session_token: session_token.clone(),
            config,
            created_at: Instant::now(),
            last_connected_at: Instant::now(),
            state: SessionState::Connected,
            replay_buffer: VecDeque::new(),
            replay_buffer_max: self.config.replay_buffer_size,
        };

        self.token_index
            .insert(session_token.clone(), session_id.clone());
        self.sessions.insert(session_id.clone(), slot);

        info!(session_id = %session_id, "session created");
        Ok((session_id, session_token))
    }

    /// Mark a session as disconnected, starting the grace period.
    pub fn disconnect(&self, session_id: &str) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.state = SessionState::Disconnected {
                since: Instant::now(),
            };
            info!(session_id = %session_id, "session disconnected, grace period started");
        }
    }

    /// Attempt to reconnect using a session token.
    ///
    /// On success, returns the session ID, marks it connected, and
    /// issues a new token (the old one is consumed).
    pub fn reconnect(
        &self,
        old_token: &str,
    ) -> Result<(String, String, Vec<ServerMessage>), TransportError> {
        // Look up session_id by token.
        let session_id = self
            .token_index
            .remove(old_token)
            .map(|(_, id)| id)
            .ok_or_else(|| {
                TransportError::InvalidSession("unknown or expired session token".into())
            })?;

        // Get the session slot.
        let mut entry = self
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| TransportError::InvalidSession("session no longer exists".into()))?;

        // Check it's actually disconnected and within grace.
        match &entry.state {
            SessionState::Connected => {
                // Already connected -- reject duplicate reconnection.
                // Re-insert the token since we removed it.
                self.token_index
                    .insert(old_token.to_owned(), session_id.clone());
                return Err(TransportError::InvalidSession(
                    "session is already connected".into(),
                ));
            }
            SessionState::Disconnected { since } => {
                if since.elapsed() > self.config.reconnect_grace_period {
                    return Err(TransportError::InvalidSession(
                        "session grace period expired".into(),
                    ));
                }
            }
        }

        // Reconnect: mark connected, issue new token, drain replay buffer.
        entry.state = SessionState::Connected;
        entry.last_connected_at = Instant::now();

        let new_token = generate_token();
        entry.session_token.clone_from(&new_token);

        let replay = entry.drain_replay_buffer();

        self.token_index
            .insert(new_token.clone(), session_id.clone());

        info!(
            session_id = %session_id,
            replay_count = replay.len(),
            "session reconnected"
        );

        Ok((session_id, new_token, replay))
    }

    /// Buffer a server message for a disconnected session.
    pub fn buffer_message(&self, session_id: &str, msg: ServerMessage) {
        if let Some(mut entry) = self.sessions.get_mut(session_id)
            && matches!(entry.state, SessionState::Disconnected { .. })
        {
            entry.buffer_message(msg);
        }
    }

    /// Remove a session entirely.
    pub fn remove(&self, session_id: &str) {
        if let Some((_, slot)) = self.sessions.remove(session_id) {
            self.token_index.remove(&slot.session_token);
            debug!(session_id = %session_id, "session removed");
        }
    }

    /// Get a reference to a session's config.
    pub fn get_config(&self, session_id: &str) -> Option<SessionConfig> {
        self.sessions.get(session_id).map(|e| e.config.clone())
    }

    /// Check if a session exists and is connected.
    pub fn is_connected(&self, session_id: &str) -> bool {
        self.sessions
            .get(session_id)
            .is_some_and(|e| matches!(e.state, SessionState::Connected))
    }

    /// Number of active sessions.
    pub fn active_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|e| matches!(e.state, SessionState::Connected))
            .count()
    }

    /// Total sessions (active + disconnected, not yet reaped).
    pub fn total_count(&self) -> usize {
        self.sessions.len()
    }

    /// Reap expired sessions.  Returns the number of sessions removed.
    pub fn reap_expired(&self) -> usize {
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|e| e.is_expired(self.config.reconnect_grace_period))
            .map(|e| e.session_id.clone())
            .collect();

        let count = expired.len();
        for session_id in &expired {
            self.remove(session_id);
        }

        if count > 0 {
            info!(count = count, "reaped expired sessions");
        }
        count
    }

    /// Evict the disconnected session that has been waiting longest, freeing one
    /// slot. Returns `true` if a session was evicted. Connected sessions are
    /// never touched. Collects the target id before removing so we never hold a
    /// `DashMap` iterator across a `remove` (which would deadlock).
    fn evict_oldest_disconnected(&self) -> bool {
        let mut oldest: Option<(String, Instant)> = None;
        for entry in self.sessions.iter() {
            if let SessionState::Disconnected { since } = &entry.state {
                let is_older = oldest.as_ref().is_none_or(|(_, t)| *since < *t);
                if is_older {
                    oldest = Some((entry.session_id.clone(), *since));
                }
            }
        }
        match oldest {
            Some((id, _)) => {
                self.remove(&id);
                debug!(session_id = %id, "evicted oldest disconnected session to free capacity");
                true
            }
            None => false,
        }
    }

    /// Spawn a background task that periodically reaps expired sessions.
    pub fn spawn_reaper(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let store = Arc::clone(self);
        let interval = store.config.reap_interval;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                store.reap_expired();
            }
        })
    }
}

/// Generate a cryptographically random session token (hex-encoded).
fn generate_token() -> String {
    // uuid v4 uses the OS CSPRNG. Concatenate two for 256 bits of entropy.
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    format!("{}{}", a.as_simple(), b.as_simple())
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/session_store.rs"]
mod tests;
