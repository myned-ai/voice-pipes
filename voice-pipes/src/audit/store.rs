//! Pluggable storage backends for audit blobs and session history.
//!
//! # Audit store
//!
//! [`AuditStore`] abstracts how audit artifacts (WAV files, JSON summaries,
//! text logs) are persisted.  The default implementation is
//! [`LocalFsAuditStore`] which writes to the local filesystem, but the
//! trait can be implemented for cloud object stores, databases, etc.
//!
//! # Session store
//!
//! [`SessionStore`] abstracts how session turn records are stored and
//! queried.  The default implementation is [`InMemorySessionStore`] which
//! keeps entries in a bounded `VecDeque`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use voice_pipes_sdk::TenantId;

use crate::api::types::{SessionRecord, SessionStatus, TurnRecord};

// ── Blob metadata ───────────────────────────────────────────

/// Metadata for a stored blob returned by [`AuditStore::list`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredBlob {
    /// Key (logical path) of the blob -- e.g. `"session-1/001_mic_raw.wav"`.
    pub key: String,
    pub size_bytes: u64,
    pub created_at: Option<DateTime<Utc>>,
}

// ── AuditStore trait ────────────────────────────────────────

/// Pluggable storage backend for audit artifacts (WAV files, JSON, text
/// records).  Keys use `'/'` as a logical separator; implementations
/// translate keys to whatever addressing scheme they need.
#[async_trait]
pub trait AuditStore: Send + Sync + std::fmt::Debug + 'static {
    /// Store bytes under the given key.
    async fn put(&self, key: &str, data: &[u8]) -> anyhow::Result<()>;

    /// Retrieve bytes for a key.  Returns `None` if the key does not exist.
    async fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>>;

    /// List blobs whose key starts with `prefix`.  Pass `""` to list the
    /// root level.  Only direct children are returned (no recursive descent).
    async fn list(&self, prefix: &str) -> anyhow::Result<Vec<StoredBlob>>;

    /// Check whether a key exists.
    async fn exists(&self, key: &str) -> anyhow::Result<bool>;

    /// Append bytes to the given key. Creates the file if it does not exist.
    async fn append(&self, key: &str, data: &[u8]) -> anyhow::Result<()>;

    /// Remove a blob; `true` when it existed. The default removes nothing,
    /// for a store that cannot delete.
    async fn delete(&self, key: &str) -> anyhow::Result<bool> {
        let _ = key;
        Ok(false)
    }

    /// Open a blob for streaming reads. Returns `None` if the key does
    /// not exist. The default loads the blob with [`get`](Self::get);
    /// file-backed stores stream from disk instead.
    async fn open(&self, key: &str) -> anyhow::Result<Option<BlobReader>> {
        Ok(self
            .get(key)
            .await?
            .map(|bytes| Box::new(std::io::Cursor::new(bytes)) as BlobReader))
    }
}

/// A streaming handle on one blob.
pub type BlobReader = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

// ── SessionStore trait ──────────────────────────────────────

/// AND-combined filter set for paged session listings
/// ([`SessionStore::sessions_page`]).
#[derive(Debug, Default, Clone)]
pub struct SessionFilter {
    pub tenant_id: Option<TenantId>,
    pub status: Option<SessionStatus>,
    pub worker_id: Option<String>,
    pub pipeline_name: Option<String>,
    /// Exact-or-prefix match on `pipeline_config_hash`.
    pub pipeline_hash_prefix: Option<String>,
    /// Only sessions carrying this tag.
    pub tag: Option<String>,
    /// Only sessions NOT carrying this tag.
    pub exclude_tag: Option<String>,
    /// Only sessions with at least this many turns.
    pub min_turns: Option<u32>,
    /// Only sessions started at or after this instant.
    pub started_after: Option<chrono::DateTime<chrono::Utc>>,
    /// Only sessions started at or before this instant.
    pub started_before: Option<chrono::DateTime<chrono::Utc>>,
}

impl SessionFilter {
    /// In-memory equivalent of the SQL pushdown in
    /// `SqliteSessionStore::sessions_page`; used by the default trait
    /// implementation. Keep the two in sync.
    fn matches(&self, s: &SessionRecord) -> bool {
        self.tenant_id
            .as_ref()
            .is_none_or(|t| s.tenant_id.as_ref() == Some(t))
            && self.status.is_none_or(|st| s.status == st)
            && self.worker_id.as_deref().is_none_or(|w| s.worker_id == w)
            && self
                .pipeline_name
                .as_deref()
                .is_none_or(|n| s.pipeline_name.as_deref() == Some(n))
            && self.pipeline_hash_prefix.as_deref().is_none_or(|h| {
                s.pipeline_config_hash
                    .as_deref()
                    .is_some_and(|v| v.starts_with(h))
            })
            && self
                .tag
                .as_deref()
                .is_none_or(|t| s.tags.iter().any(|x| x == t))
            && self
                .exclude_tag
                .as_deref()
                .is_none_or(|t| !s.tags.iter().any(|x| x == t))
            && self.min_turns.is_none_or(|m| s.turn_count >= m)
            && self.started_after.is_none_or(|t| s.started_at >= t)
            && self.started_before.is_none_or(|t| s.started_at <= t)
    }
}

/// AND-combined filter set for paged turn listings
/// ([`SessionStore::turns_page`]).
#[derive(Debug, Default, Clone)]
pub struct TurnFilter {
    pub status: Option<SessionStatus>,
    pub worker_id: Option<String>,
    /// Case-insensitive substring of `text_preview`.
    pub search: Option<String>,
    pub pipeline_name: Option<String>,
    /// Exact-or-prefix match on `pipeline_config_hash`.
    pub pipeline_hash_prefix: Option<String>,
    pub tag: Option<String>,
    pub exclude_tag: Option<String>,
}

impl TurnFilter {
    /// In-memory equivalent of the SQL pushdown in
    /// `SqliteSessionStore::turns_page`. Keep the two in sync.
    fn matches(&self, t: &TurnRecord) -> bool {
        self.status.is_none_or(|st| t.status == st)
            && self.worker_id.as_deref().is_none_or(|w| t.worker_id == w)
            && self.search.as_deref().is_none_or(|q| {
                let q = q.to_lowercase();
                t.text_preview
                    .as_deref()
                    .is_some_and(|p| p.to_lowercase().contains(&q))
            })
            && self
                .pipeline_name
                .as_deref()
                .is_none_or(|n| t.pipeline_name.as_deref() == Some(n))
            && self.pipeline_hash_prefix.as_deref().is_none_or(|h| {
                t.pipeline_config_hash
                    .as_deref()
                    .is_some_and(|v| v.starts_with(h))
            })
            && self
                .tag
                .as_deref()
                .is_none_or(|x| t.tags.iter().any(|y| y == x))
            && self
                .exclude_tag
                .as_deref()
                .is_none_or(|x| !t.tags.iter().any(|y| y == x))
    }

    /// True when every set criterion has a column in the sqlite `turns`
    /// table, so the whole listing can be a single indexed query.
    #[cfg(feature = "session-sqlite")]
    fn is_sql_only(&self) -> bool {
        self.search.is_none()
            && self.pipeline_name.is_none()
            && self.pipeline_hash_prefix.is_none()
            && self.tag.is_none()
            && self.exclude_tag.is_none()
    }
}

/// Filter and page a locked in-memory turn deque without cloning the
/// records that fall outside the page.
fn page_turns<'a>(
    turns: impl Iterator<Item = &'a TurnRecord>,
    filter: &TurnFilter,
    limit: usize,
    offset: usize,
) -> (Vec<TurnRecord>, usize) {
    let mut total = 0;
    let mut items = Vec::new();
    for t in turns.filter(|t| filter.matches(t)) {
        if total >= offset && items.len() < limit {
            items.push(t.clone());
        }
        total += 1;
    }
    (items, total)
}

/// Pluggable storage backend for sessions and turn records.
#[async_trait]
pub trait SessionStore: Send + Sync + std::fmt::Debug + 'static {
    // ── Session-level ───────────────────────────────────

    /// Record or update a session (upsert by id).
    async fn push_session(&self, record: SessionRecord);

    /// Return sessions, newest first. Optional filter by status.
    async fn sessions(
        &self,
        status: Option<SessionStatus>,
        limit: usize,
        offset: usize,
    ) -> Vec<SessionRecord>;

    /// Paged, filtered listing, newest first: one page of sessions plus
    /// the total match count (for pagination UIs). The default
    /// implementation filters a full `sessions()` sweep in memory,
    /// which is fine for the bounded Fs/InMemory stores;
    /// `SqliteSessionStore` overrides it with SQL pushdown so listings
    /// never load the whole table.
    async fn sessions_page(
        &self,
        filter: &SessionFilter,
        limit: usize,
        offset: usize,
    ) -> (Vec<SessionRecord>, usize) {
        let all = self.sessions(filter.status, usize::MAX, 0).await;
        let matched: Vec<SessionRecord> = all.into_iter().filter(|s| filter.matches(s)).collect();
        let total = matched.len();
        let items = matched.into_iter().skip(offset).take(limit).collect();
        (items, total)
    }

    /// Look up a single session by ID.
    async fn find_session(&self, id: &str) -> Option<SessionRecord>;

    // ── Turn-level ──────────────────────────────────────

    /// Record a completed turn, evicting the oldest entry when at capacity.
    async fn push(&self, record: TurnRecord);

    /// Return **all** turn entries (newest first).
    async fn entries(&self) -> Vec<TurnRecord>;

    /// Turns that started at or after `cutoff`, optionally for one worker,
    /// newest first. The rolling-window metrics call this on every poll,
    /// so stores keep the work proportional to the window, not the table:
    /// the default filters a full sweep and is only for stores without a
    /// cheaper path.
    async fn turns_since(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        worker_id: Option<&str>,
    ) -> Vec<TurnRecord> {
        self.entries()
            .await
            .into_iter()
            .filter(|t| t.started_at >= cutoff && worker_id.is_none_or(|w| t.worker_id == w))
            .collect()
    }

    /// Paged, filtered turn listing, newest first, plus the total match
    /// count. As with [`sessions_page`](Self::sessions_page), the default
    /// filters a full sweep; stores override it so a page never clones the
    /// whole table.
    async fn turns_page(
        &self,
        filter: &TurnFilter,
        limit: usize,
        offset: usize,
    ) -> (Vec<TurnRecord>, usize) {
        let matched: Vec<TurnRecord> = self
            .entries()
            .await
            .into_iter()
            .filter(|t| filter.matches(t))
            .collect();
        let total = matched.len();
        let items = matched.into_iter().skip(offset).take(limit).collect();
        (items, total)
    }

    /// Return turns for a specific session, ordered by `started_at`.
    async fn turns_for_session(&self, session_id: &str) -> Vec<TurnRecord>;

    /// Look up a single turn by ID.
    async fn find(&self, id: &str) -> Option<TurnRecord>;

    /// Total number of turns recorded since startup.
    async fn total_processed(&self) -> u64;

    /// Total number of turns that ended in error.
    async fn errors_total(&self) -> u64;
}

// ── LocalFsAuditStore ───────────────────────────────────────

/// Stores audit blobs on the local filesystem under a base directory.
/// Keys containing `'/'` create subdirectories automatically.
#[derive(Debug)]
pub struct LocalFsAuditStore {
    base_dir: PathBuf,
}

impl LocalFsAuditStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    /// Resolve `key` under the base dir, rejecting anything that could
    /// escape it. Keys are validated **here at the store
    /// boundary**, not in HTTP handlers, so every caller gets the same
    /// guarantee: relative forward-slash paths only — no `..`, no
    /// absolute paths, no drive/UNC prefixes, no backslashes.
    fn safe_key_path(&self, key: &str) -> anyhow::Result<PathBuf> {
        validate_blob_key(key)?;
        Ok(self.base_dir.join(key))
    }
}

/// Shared blob-key validation for filesystem-backed stores.
pub fn validate_blob_key(key: &str) -> anyhow::Result<()> {
    if key.is_empty() {
        anyhow::bail!("blob key must not be empty");
    }
    if key.contains('\\') || key.contains(':') || key.starts_with('/') {
        anyhow::bail!("invalid blob key {key:?}: absolute paths and backslashes are not allowed");
    }
    for component in key.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            anyhow::bail!(
                "invalid blob key {key:?}: empty, `.` or `..` path components are not allowed"
            );
        }
    }
    Ok(())
}

#[async_trait]
impl AuditStore for LocalFsAuditStore {
    /// Write to a sibling temporary file and rename it into place, so a
    /// reader (or a crash mid-write) never sees a partial blob.
    async fn put(&self, key: &str, data: &[u8]) -> anyhow::Result<()> {
        let path = self.safe_key_path(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp_name = format!(
            ".{}.{}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("blob"),
            uuid::Uuid::new_v4().simple()
        );
        let tmp = path.with_file_name(tmp_name);
        let written = async {
            tokio::fs::write(&tmp, data).await?;
            tokio::fs::rename(&tmp, &path).await
        }
        .await;
        if written.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        written?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> anyhow::Result<bool> {
        let path = self.safe_key_path(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let path = self.safe_key_path(key)?;
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn open(&self, key: &str) -> anyhow::Result<Option<BlobReader>> {
        let path = self.safe_key_path(key)?;
        match tokio::fs::File::open(&path).await {
            Ok(file) => Ok(Some(Box::new(file) as BlobReader)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list(&self, prefix: &str) -> anyhow::Result<Vec<StoredBlob>> {
        let dir = if prefix.is_empty() {
            self.base_dir.clone()
        } else {
            self.safe_key_path(prefix.strip_suffix('/').unwrap_or(prefix))?
        };

        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };

        let mut blobs = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }

            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            let key = if prefix.is_empty() {
                name
            } else {
                let pfx = prefix.strip_suffix('/').unwrap_or(prefix);
                format!("{pfx}/{name}")
            };

            let meta = entry.metadata().await.ok();
            let size_bytes = meta.as_ref().map_or(0, std::fs::Metadata::len);
            // Not every filesystem records a creation time; the last
            // modification stands in for it.
            let created_at = meta
                .and_then(|m| m.created().or_else(|_| m.modified()).ok())
                .map(DateTime::<Utc>::from);

            blobs.push(StoredBlob {
                key,
                size_bytes,
                created_at,
            });
        }

        Ok(blobs)
    }

    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        let path = self.safe_key_path(key)?;
        Ok(tokio::fs::try_exists(&path).await?)
    }

    async fn append(&self, key: &str, data: &[u8]) -> anyhow::Result<()> {
        let path = self.safe_key_path(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(data).await?;
        // `write_all` on a tokio file returns once the bytes are handed to a
        // blocking thread, not once they are written, and dropping the handle
        // does not wait either. Without this the next append opens a second
        // handle while this write is still in flight. Two handles appending
        // to one file is only safe where the filesystem makes appends atomic,
        // which a container's bind mount does not: one record lands on top
        // of part of another, leaving a torn line and a lost event. Flushing
        // also surfaces a failed write here instead of discarding it on drop.
        file.flush().await?;
        Ok(())
    }
}

// ── FsSessionStore ──────────────────────────────────────────

/// Persists sessions and turn records as JSON files on the local filesystem.
///
/// Layout:
///   `<base_dir>/sessions/<session_id>.json`
///   `<base_dir>/turns/<turn_id>.json`
///
/// An in-memory index (loaded at startup) provides fast lookups.
/// Writes are flushed to disk on every mutation.
#[derive(Debug)]
pub struct FsSessionStore {
    base_dir: PathBuf,
    sessions: parking_lot::RwLock<VecDeque<SessionRecord>>,
    turns: parking_lot::RwLock<VecDeque<TurnRecord>>,
    max_entries: usize,
    total_processed: AtomicU64,
    errors_total: AtomicU64,
}

impl FsSessionStore {
    /// Create a new store, loading any existing data from disk.
    pub fn new(base_dir: PathBuf, max_entries: usize) -> Self {
        let sessions_dir = base_dir.join("sessions");
        let turns_dir = base_dir.join("turns");
        let _ = std::fs::create_dir_all(&sessions_dir);
        let _ = std::fs::create_dir_all(&turns_dir);

        let mut sessions: Vec<SessionRecord> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
            for entry in rd.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json")
                    && let Ok(data) = std::fs::read(entry.path())
                    && let Ok(rec) = serde_json::from_slice::<SessionRecord>(&data)
                {
                    sessions.push(rec);
                }
            }
        }
        // Sort newest first by started_at, keep the window, and remove
        // the files of records that fall outside it so the directory
        // does not grow across restarts.
        sessions.sort_by_key(|b| std::cmp::Reverse(b.started_at));
        for evicted in sessions.drain(max_entries.min(sessions.len())..) {
            let _ = std::fs::remove_file(sessions_dir.join(format!("{}.json", evicted.id)));
        }

        let mut turns: Vec<TurnRecord> = Vec::new();
        let mut total_processed: u64 = 0;
        let mut errors_total: u64 = 0;
        if let Ok(rd) = std::fs::read_dir(&turns_dir) {
            for entry in rd.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json")
                    && let Ok(data) = std::fs::read(entry.path())
                    && let Ok(rec) = serde_json::from_slice::<TurnRecord>(&data)
                {
                    total_processed += 1;
                    if rec.status == SessionStatus::Error {
                        errors_total += 1;
                    }
                    turns.push(rec);
                }
            }
        }
        // Sort newest first by started_at; trim to max_entries and
        // remove the trimmed records' files.
        turns.sort_by_key(|b| std::cmp::Reverse(b.started_at));
        for evicted in turns.drain(max_entries.min(turns.len())..) {
            let _ = std::fs::remove_file(turns_dir.join(format!("{}.json", evicted.id)));
        }

        tracing::info!(
            sessions = sessions.len(),
            turns = turns.len(),
            dir = %base_dir.display(),
            "FsSessionStore loaded"
        );

        Self {
            base_dir,
            sessions: parking_lot::RwLock::new(VecDeque::from(sessions)),
            turns: parking_lot::RwLock::new(VecDeque::from(turns)),
            max_entries,
            total_processed: AtomicU64::new(total_processed),
            errors_total: AtomicU64::new(errors_total),
        }
    }

    async fn write_session(&self, record: &SessionRecord) {
        let path = self
            .base_dir
            .join("sessions")
            .join(format!("{}.json", record.id));
        if let Ok(data) = serde_json::to_vec_pretty(record) {
            // Atomic temp+rename, offloaded to a blocking thread so the
            // filesystem write doesn't stall the async runtime.
            let _ = tokio::task::spawn_blocking(move || {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, &data).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            })
            .await;
        }
    }

    async fn write_turn(&self, record: &TurnRecord) {
        let path = self
            .base_dir
            .join("turns")
            .join(format!("{}.json", record.id));
        if let Ok(data) = serde_json::to_vec_pretty(record) {
            let _ = tokio::task::spawn_blocking(move || {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, &data).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            })
            .await;
        }
    }

    /// Remove the file of a record evicted from the in-memory window;
    /// the index is the source of truth, so a leftover file would only
    /// come back at the next load and be evicted again.
    fn remove_record_file(&self, subdir: &str, id: &str) {
        let path = self.base_dir.join(subdir).join(format!("{id}.json"));
        tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_file(path);
        });
    }
}

#[async_trait]
impl SessionStore for FsSessionStore {
    // ── Session-level ───────────────────────────────────

    async fn push_session(&self, record: SessionRecord) {
        self.write_session(&record).await;
        let evicted = {
            let mut sessions = self.sessions.write();
            if let Some(existing) = sessions.iter_mut().find(|s| s.id == record.id) {
                *existing = record;
                None
            } else {
                let evicted = if sessions.len() >= self.max_entries {
                    sessions.pop_back()
                } else {
                    None
                };
                sessions.push_front(record);
                evicted
            }
        };
        if let Some(old) = evicted {
            self.remove_record_file("sessions", &old.id);
        }
    }

    async fn sessions(
        &self,
        status: Option<SessionStatus>,
        limit: usize,
        offset: usize,
    ) -> Vec<SessionRecord> {
        let sessions = self.sessions.read();
        let iter = sessions
            .iter()
            .filter(|s| status.is_none_or(|st| s.status == st));
        iter.skip(offset).take(limit).cloned().collect()
    }

    async fn find_session(&self, id: &str) -> Option<SessionRecord> {
        self.sessions.read().iter().find(|s| s.id == id).cloned()
    }

    // ── Turn-level ──────────────────────────────────────

    async fn push(&self, record: TurnRecord) {
        let evicted = {
            let mut entries = self.turns.write();
            if let Some(existing) = entries.iter_mut().find(|t| t.id == record.id) {
                if existing.status == SessionStatus::Error && record.status != SessionStatus::Error
                {
                    self.errors_total.fetch_sub(1, Ordering::Relaxed);
                } else if existing.status != SessionStatus::Error
                    && record.status == SessionStatus::Error
                {
                    self.errors_total.fetch_add(1, Ordering::Relaxed);
                }
                *existing = record.clone();
                None
            } else {
                if record.status == SessionStatus::Error {
                    self.errors_total.fetch_add(1, Ordering::Relaxed);
                }
                self.total_processed.fetch_add(1, Ordering::Relaxed);
                let evicted = if entries.len() >= self.max_entries {
                    entries.pop_back()
                } else {
                    None
                };
                entries.push_front(record.clone());
                evicted
            }
        };
        self.write_turn(&record).await;
        if let Some(old) = evicted {
            self.remove_record_file("turns", &old.id);
        }
    }

    async fn entries(&self) -> Vec<TurnRecord> {
        self.turns.read().iter().cloned().collect()
    }

    async fn turns_since(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        worker_id: Option<&str>,
    ) -> Vec<TurnRecord> {
        self.turns
            .read()
            .iter()
            .filter(|t| t.started_at >= cutoff && worker_id.is_none_or(|w| t.worker_id == w))
            .cloned()
            .collect()
    }

    async fn turns_page(
        &self,
        filter: &TurnFilter,
        limit: usize,
        offset: usize,
    ) -> (Vec<TurnRecord>, usize) {
        page_turns(self.turns.read().iter(), filter, limit, offset)
    }

    async fn turns_for_session(&self, session_id: &str) -> Vec<TurnRecord> {
        self.turns
            .read()
            .iter()
            .filter(|t| t.session_id == session_id)
            .cloned()
            .collect()
    }

    async fn find(&self, id: &str) -> Option<TurnRecord> {
        self.turns.read().iter().find(|s| s.id == id).cloned()
    }

    async fn total_processed(&self) -> u64 {
        self.total_processed.load(Ordering::Relaxed)
    }

    async fn errors_total(&self) -> u64 {
        self.errors_total.load(Ordering::Relaxed)
    }
}

// ── InMemorySessionStore ────────────────────────────────────

/// Keeps sessions and turn records in bounded in-memory ring buffers.
#[derive(Debug)]
pub struct InMemorySessionStore {
    sessions: parking_lot::RwLock<VecDeque<SessionRecord>>,
    entries: parking_lot::RwLock<VecDeque<TurnRecord>>,
    max_entries: usize,
    total_processed: AtomicU64,
    errors_total: AtomicU64,
}

impl InMemorySessionStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            sessions: parking_lot::RwLock::new(VecDeque::new()),
            entries: parking_lot::RwLock::new(VecDeque::new()),
            max_entries,
            total_processed: AtomicU64::new(0),
            errors_total: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    // ── Session-level ───────────────────────────────────

    async fn push_session(&self, record: SessionRecord) {
        let mut sessions = self.sessions.write();
        // Upsert: if a session with this ID exists, update it in place.
        if let Some(existing) = sessions.iter_mut().find(|s| s.id == record.id) {
            *existing = record;
        } else {
            if sessions.len() >= self.max_entries {
                sessions.pop_back();
            }
            sessions.push_front(record);
        }
    }

    async fn sessions(
        &self,
        status: Option<SessionStatus>,
        limit: usize,
        offset: usize,
    ) -> Vec<SessionRecord> {
        let sessions = self.sessions.read();
        let iter = sessions
            .iter()
            .filter(|s| status.is_none_or(|st| s.status == st));
        iter.skip(offset).take(limit).cloned().collect()
    }

    async fn find_session(&self, id: &str) -> Option<SessionRecord> {
        self.sessions.read().iter().find(|s| s.id == id).cloned()
    }

    // ── Turn-level ──────────────────────────────────────

    async fn push(&self, record: TurnRecord) {
        let mut entries = self.entries.write();
        if let Some(existing) = entries.iter_mut().find(|t| t.id == record.id) {
            if existing.status == SessionStatus::Error && record.status != SessionStatus::Error {
                self.errors_total.fetch_sub(1, Ordering::Relaxed);
            } else if existing.status != SessionStatus::Error
                && record.status == SessionStatus::Error
            {
                self.errors_total.fetch_add(1, Ordering::Relaxed);
            }
            *existing = record;
        } else {
            if record.status == SessionStatus::Error {
                self.errors_total.fetch_add(1, Ordering::Relaxed);
            }
            self.total_processed.fetch_add(1, Ordering::Relaxed);
            if entries.len() >= self.max_entries {
                entries.pop_back();
            }
            entries.push_front(record);
        }
    }

    async fn entries(&self) -> Vec<TurnRecord> {
        self.entries.read().iter().cloned().collect()
    }

    async fn turns_since(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        worker_id: Option<&str>,
    ) -> Vec<TurnRecord> {
        self.entries
            .read()
            .iter()
            .filter(|t| t.started_at >= cutoff && worker_id.is_none_or(|w| t.worker_id == w))
            .cloned()
            .collect()
    }

    async fn turns_page(
        &self,
        filter: &TurnFilter,
        limit: usize,
        offset: usize,
    ) -> (Vec<TurnRecord>, usize) {
        page_turns(self.entries.read().iter(), filter, limit, offset)
    }

    async fn turns_for_session(&self, session_id: &str) -> Vec<TurnRecord> {
        self.entries
            .read()
            .iter()
            .filter(|t| t.session_id == session_id)
            .cloned()
            .collect()
    }

    async fn find(&self, id: &str) -> Option<TurnRecord> {
        self.entries.read().iter().find(|s| s.id == id).cloned()
    }

    async fn total_processed(&self) -> u64 {
        self.total_processed.load(Ordering::Relaxed)
    }

    async fn errors_total(&self) -> u64 {
        self.errors_total.load(Ordering::Relaxed)
    }
}

// ── SqliteSessionStore ──────────────────────────────────────

#[cfg(feature = "session-sqlite")]
pub use sqlite::SqliteSessionStore;

#[cfg(feature = "session-sqlite")]
mod sqlite {
    use rusqlite::{Connection, OptionalExtension, params};

    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{SessionFilter, SessionStatus, SessionStore, TurnFilter, page_turns};
    use crate::api::types::{SessionRecord, TurnRecord};

    /// SQLite-backed session/turn store.
    ///
    /// Each record is stored both as denormalized, indexed columns (for fast
    /// filtered/sorted queries) and as its full JSON blob in a `data` column
    /// (the lossless source of truth, so new `SessionRecord`/`TurnRecord` fields
    /// round-trip without a schema migration). History is retained in full —
    /// `max_entries` only bounds the in-memory window returned by [`entries`],
    /// never what is persisted.
    ///
    /// [`entries`]: SqliteSessionStore::entries
    #[derive(Debug)]
    pub struct SqliteSessionStore {
        // Arc so query closures can move a handle into `spawn_blocking`,
        // keeping the synchronous rusqlite work off the async runtime threads.
        conn: Arc<parking_lot::Mutex<Connection>>,
        max_entries: usize,
    }

    /// Escape `%`, `_` and `\` so a value can be embedded in a `LIKE`
    /// pattern used with `ESCAPE '\'`.
    fn escape_like(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    }

    /// Maps [`SessionStatus`] to the same `snake_case` string serde emits, so the
    /// indexed `status` column matches the values used in JSON and query filters.
    fn status_str(status: SessionStatus) -> &'static str {
        match status {
            SessionStatus::InProgress => "in_progress",
            SessionStatus::Completed => "completed",
            SessionStatus::Error => "error",
        }
    }

    impl SqliteSessionStore {
        /// Open (creating if absent) the database at `path` and ensure the schema.
        pub fn new(path: PathBuf, max_entries: usize) -> anyhow::Result<Self> {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                let _ = std::fs::create_dir_all(parent);
            }
            let conn = Connection::open(&path)?;
            // WAL gives readers/writers concurrency and a durable, crash-safe log;
            // NORMAL sync is the standard WAL pairing (durable across app crashes,
            // fsync only at checkpoint).
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                 id                   TEXT PRIMARY KEY,
                 tenant_id            TEXT,
                 worker_id            TEXT NOT NULL,
                 started_at           TEXT NOT NULL,
                 ended_at             TEXT,
                 status               TEXT NOT NULL,
                 pipeline_name        TEXT,
                 pipeline_config_hash TEXT,
                 tags                 TEXT NOT NULL DEFAULT '[]',
                 turn_count           INTEGER NOT NULL DEFAULT 0,
                 data                 TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_sessions_status  ON sessions(status);
             CREATE INDEX IF NOT EXISTS idx_sessions_worker  ON sessions(worker_id);

             CREATE TABLE IF NOT EXISTS turns (
                 id          TEXT PRIMARY KEY,
                 session_id  TEXT NOT NULL,
                 worker_id   TEXT NOT NULL,
                 started_at  TEXT NOT NULL,
                 status      TEXT NOT NULL,
                 data        TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);
             CREATE INDEX IF NOT EXISTS idx_turns_started ON turns(started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_turns_status  ON turns(status);",
            )?;

            // Idempotent column addition and index for existing sqlite databases created before multi-tenancy.
            let _ = conn.execute("ALTER TABLE sessions ADD COLUMN tenant_id TEXT", []);
            let _ = conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_sessions_tenant ON sessions(tenant_id)",
                [],
            );

            let (sessions, turns): (i64, i64) = {
                let s = conn
                    .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
                    .unwrap_or(0);
                let t = conn
                    .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
                    .unwrap_or(0);
                (s, t)
            };
            tracing::info!(sessions, turns, path = %path.display(), "SqliteSessionStore opened");

            Ok(Self {
                conn: Arc::new(parking_lot::Mutex::new(conn)),
                max_entries,
            })
        }

        /// Deserialize the `data` JSON column for each row of a query.
        fn query_json<T: serde::de::DeserializeOwned>(
            conn: &Connection,
            sql: &str,
            params: &[&dyn rusqlite::ToSql],
        ) -> Vec<T> {
            let mut stmt = match conn.prepare(sql) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "sqlite prepare failed");
                    return Vec::new();
                }
            };
            let rows = stmt.query_map(params, |row| row.get::<_, String>(0));
            let rows = match rows {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "sqlite query failed");
                    return Vec::new();
                }
            };
            rows.filter_map(std::result::Result::ok)
                .filter_map(|json| serde_json::from_str::<T>(&json).ok())
                .collect()
        }
    }

    #[async_trait]
    impl SessionStore for SqliteSessionStore {
        // ── Session-level ───────────────────────────────────

        async fn push_session(&self, record: SessionRecord) {
            let data = match serde_json::to_string(&record) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, "serialize SessionRecord failed");
                    return;
                }
            };
            let tags = serde_json::to_string(&record.tags).unwrap_or_else(|_| "[]".into());
            let conn = Arc::clone(&self.conn);
            let _ = tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                let res = conn.execute(
                    "INSERT INTO sessions
                     (id, tenant_id, worker_id, started_at, ended_at, status, pipeline_name,
                      pipeline_config_hash, tags, turn_count, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                     tenant_id = excluded.tenant_id,
                     worker_id = excluded.worker_id,
                     started_at = excluded.started_at,
                     ended_at = excluded.ended_at,
                     status = excluded.status,
                     pipeline_name = excluded.pipeline_name,
                     pipeline_config_hash = excluded.pipeline_config_hash,
                     tags = excluded.tags,
                     turn_count = excluded.turn_count,
                     data = excluded.data",
                    params![
                        record.id,
                        record
                            .tenant_id
                            .as_ref()
                            .map(voice_pipes_sdk::TenantId::as_str),
                        record.worker_id,
                        record.started_at.to_rfc3339(),
                        record.ended_at.map(|t| t.to_rfc3339()),
                        status_str(record.status),
                        record.pipeline_name,
                        record.pipeline_config_hash,
                        tags,
                        record.turn_count,
                        data,
                    ],
                );
                if let Err(e) = res {
                    tracing::warn!(error = %e, "sqlite push_session failed");
                }
            })
            .await;
        }

        async fn sessions(
            &self,
            status: Option<SessionStatus>,
            limit: usize,
            offset: usize,
        ) -> Vec<SessionRecord> {
            let conn = Arc::clone(&self.conn);
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                match status {
                    Some(st) => Self::query_json(
                        &conn,
                        "SELECT data FROM sessions WHERE status = ?1
                     ORDER BY started_at DESC LIMIT ?2 OFFSET ?3",
                        params![status_str(st), limit as i64, offset as i64],
                    ),
                    None => Self::query_json(
                        &conn,
                        "SELECT data FROM sessions ORDER BY started_at DESC LIMIT ?1 OFFSET ?2",
                        params![limit as i64, offset as i64],
                    ),
                }
            })
            .await
            .unwrap_or_default()
        }

        async fn sessions_page(
            &self,
            filter: &SessionFilter,
            limit: usize,
            offset: usize,
        ) -> (Vec<SessionRecord>, usize) {
            let filter = filter.clone();
            let conn = Arc::clone(&self.conn);
            tokio::task::spawn_blocking(move || {
                let mut clauses: Vec<&'static str> = Vec::new();
                let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                if let Some(t) = filter.tenant_id {
                    clauses.push("tenant_id = ?");
                    args.push(Box::new(t.to_string()));
                }
                if let Some(st) = filter.status {
                    clauses.push("status = ?");
                    args.push(Box::new(status_str(st).to_string()));
                }
                if let Some(w) = filter.worker_id {
                    clauses.push("worker_id = ?");
                    args.push(Box::new(w));
                }
                if let Some(n) = filter.pipeline_name {
                    clauses.push("pipeline_name = ?");
                    args.push(Box::new(n));
                }
                if let Some(h) = filter.pipeline_hash_prefix {
                    clauses.push("pipeline_config_hash LIKE ? ESCAPE '\\'");
                    args.push(Box::new(format!("{}%", escape_like(&h))));
                }
                // `tags` holds the serde-serialized JSON array, so a tag match
                // is a substring match on the quoted element (`"benchmark"`);
                // the closing quote anchors it against prefix collisions.
                if let Some(t) = filter.tag {
                    let quoted = serde_json::to_string(&t).unwrap_or_default();
                    clauses.push("tags LIKE ? ESCAPE '\\'");
                    args.push(Box::new(format!("%{}%", escape_like(&quoted))));
                }
                if let Some(t) = filter.exclude_tag {
                    let quoted = serde_json::to_string(&t).unwrap_or_default();
                    clauses.push("tags NOT LIKE ? ESCAPE '\\'");
                    args.push(Box::new(format!("%{}%", escape_like(&quoted))));
                }
                if let Some(m) = filter.min_turns {
                    clauses.push("turn_count >= ?");
                    args.push(Box::new(m as i64));
                }
                // `started_at` is stored as RFC3339 with a fixed `+00:00`
                // offset, so string comparison orders chronologically
                // (sub-second edge cases don't matter for range filters).
                if let Some(t) = filter.started_after {
                    clauses.push("started_at >= ?");
                    args.push(Box::new(t.to_rfc3339()));
                }
                if let Some(t) = filter.started_before {
                    clauses.push("started_at <= ?");
                    args.push(Box::new(t.to_rfc3339()));
                }
                let where_sql = if clauses.is_empty() {
                    String::new()
                } else {
                    format!(" WHERE {}", clauses.join(" AND "))
                };

                let conn = conn.lock();
                let refs: Vec<&dyn rusqlite::ToSql> =
                    args.iter().map(std::convert::AsRef::as_ref).collect();
                let total = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM sessions{where_sql}"),
                        refs.as_slice(),
                        |r| r.get::<_, i64>(0),
                    )
                    .map_or_else(
                        |e| {
                            tracing::warn!(error = %e, "sqlite sessions_page count failed");
                            0
                        },
                        |n| n as usize,
                    );
                let limit_p = limit as i64;
                let offset_p = offset as i64;
                let mut page_refs = refs;
                page_refs.push(&limit_p);
                page_refs.push(&offset_p);
                let items = Self::query_json(
                    &conn,
                    &format!(
                        "SELECT data FROM sessions{where_sql}
                     ORDER BY started_at DESC LIMIT ? OFFSET ?"
                    ),
                    page_refs.as_slice(),
                );
                (items, total)
            })
            .await
            .unwrap_or_default()
        }

        async fn find_session(&self, id: &str) -> Option<SessionRecord> {
            let conn = Arc::clone(&self.conn);
            let id = id.to_string();
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                conn.query_row(
                    "SELECT data FROM sessions WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
                .and_then(|json| serde_json::from_str(&json).ok())
            })
            .await
            .unwrap_or(None)
        }

        // ── Turn-level ──────────────────────────────────────

        async fn push(&self, record: TurnRecord) {
            let data = match serde_json::to_string(&record) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, "serialize TurnRecord failed");
                    return;
                }
            };
            let conn = Arc::clone(&self.conn);
            let _ = tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                let res = conn.execute(
                    "INSERT INTO turns (id, session_id, worker_id, started_at, status, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                     session_id = excluded.session_id,
                     worker_id = excluded.worker_id,
                     started_at = excluded.started_at,
                     status = excluded.status,
                     data = excluded.data",
                    params![
                        record.id,
                        record.session_id,
                        record.worker_id,
                        record.started_at.to_rfc3339(),
                        status_str(record.status),
                        data,
                    ],
                );
                if let Err(e) = res {
                    tracing::warn!(error = %e, "sqlite push turn failed");
                }
            })
            .await;
        }

        async fn entries(&self) -> Vec<TurnRecord> {
            let conn = Arc::clone(&self.conn);
            let max_entries = self.max_entries;
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                Self::query_json(
                    &conn,
                    "SELECT data FROM turns ORDER BY started_at DESC LIMIT ?1",
                    params![max_entries as i64],
                )
            })
            .await
            .unwrap_or_default()
        }

        async fn turns_since(
            &self,
            cutoff: chrono::DateTime<chrono::Utc>,
            worker_id: Option<&str>,
        ) -> Vec<TurnRecord> {
            let conn = Arc::clone(&self.conn);
            let max_entries = self.max_entries;
            let cutoff = cutoff.to_rfc3339();
            let worker_id = worker_id.map(str::to_string);
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                match worker_id {
                    Some(w) => Self::query_json(
                        &conn,
                        "SELECT data FROM turns WHERE started_at >= ?1 AND worker_id = ?2
                     ORDER BY started_at DESC LIMIT ?3",
                        params![cutoff, w, max_entries as i64],
                    ),
                    None => Self::query_json(
                        &conn,
                        "SELECT data FROM turns WHERE started_at >= ?1
                     ORDER BY started_at DESC LIMIT ?2",
                        params![cutoff, max_entries as i64],
                    ),
                }
            })
            .await
            .unwrap_or_default()
        }

        async fn turns_page(
            &self,
            filter: &TurnFilter,
            limit: usize,
            offset: usize,
        ) -> (Vec<TurnRecord>, usize) {
            let conn = Arc::clone(&self.conn);
            let max_entries = self.max_entries;
            let filter = filter.clone();
            tokio::task::spawn_blocking(move || {
                // Status and worker are columns; the rest lives in the JSON
                // blob and is matched in Rust over the column-narrowed rows.
                let mut clauses: Vec<&str> = Vec::new();
                let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                if let Some(st) = filter.status {
                    clauses.push("status = ?");
                    args.push(Box::new(status_str(st).to_string()));
                }
                if let Some(w) = filter.worker_id.clone() {
                    clauses.push("worker_id = ?");
                    args.push(Box::new(w));
                }
                let where_sql = if clauses.is_empty() {
                    String::new()
                } else {
                    format!(" WHERE {}", clauses.join(" AND "))
                };
                let conn = conn.lock();
                let refs: Vec<&dyn rusqlite::ToSql> =
                    args.iter().map(std::convert::AsRef::as_ref).collect();
                if filter.is_sql_only() {
                    let total = conn
                        .query_row(
                            &format!("SELECT COUNT(*) FROM turns{where_sql}"),
                            refs.as_slice(),
                            |r| r.get::<_, i64>(0),
                        )
                        .map_or(0, |n| n as usize);
                    let limit_p = limit as i64;
                    let offset_p = offset as i64;
                    let mut page_refs = refs;
                    page_refs.push(&limit_p);
                    page_refs.push(&offset_p);
                    let items = Self::query_json(
                        &conn,
                        &format!(
                            "SELECT data FROM turns{where_sql}
                         ORDER BY started_at DESC LIMIT ? OFFSET ?"
                        ),
                        page_refs.as_slice(),
                    );
                    (items, total)
                } else {
                    let limit_p = max_entries as i64;
                    let mut all_refs = refs;
                    all_refs.push(&limit_p);
                    let rows: Vec<TurnRecord> = Self::query_json(
                        &conn,
                        &format!(
                            "SELECT data FROM turns{where_sql} ORDER BY started_at DESC LIMIT ?"
                        ),
                        all_refs.as_slice(),
                    );
                    page_turns(rows.iter(), &filter, limit, offset)
                }
            })
            .await
            .unwrap_or_default()
        }

        async fn turns_for_session(&self, session_id: &str) -> Vec<TurnRecord> {
            let conn = Arc::clone(&self.conn);
            let session_id = session_id.to_string();
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                Self::query_json(
                    &conn,
                    "SELECT data FROM turns WHERE session_id = ?1 ORDER BY started_at DESC",
                    params![session_id],
                )
            })
            .await
            .unwrap_or_default()
        }

        async fn find(&self, id: &str) -> Option<TurnRecord> {
            let conn = Arc::clone(&self.conn);
            let id = id.to_string();
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                conn.query_row("SELECT data FROM turns WHERE id = ?1", params![id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
                .ok()
                .flatten()
                .and_then(|json| serde_json::from_str(&json).ok())
            })
            .await
            .unwrap_or(None)
        }

        async fn total_processed(&self) -> u64 {
            let conn = Arc::clone(&self.conn);
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                conn.query_row("SELECT COUNT(*) FROM turns", [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0) as u64
            })
            .await
            .unwrap_or(0)
        }

        async fn errors_total(&self) -> u64 {
            let conn = Arc::clone(&self.conn);
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                conn.query_row(
                    "SELECT COUNT(*) FROM turns WHERE status = 'error'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0) as u64
            })
            .await
            .unwrap_or(0)
        }
    }
}

// ── Capture reconciliation ──────────────────────────────────

/// Backfill the session index from capture directories on disk.
///
/// Scans `captures_dir` for per-session subdirectories and, for any session
/// not already present in `store`, reconstructs a minimal [`SessionRecord`]
/// from that session's `metadata.json`. This recovers sessions whose
/// lifecycle events never reached (or were lost by) the orchestrator — e.g.
/// an in-memory index wiped on restart, or events dropped while the
/// orchestrator was down — so their captured audio/transcripts stay
/// discoverable in the admin UI instead of becoming orphaned on disk.
///
/// Recovered sessions are tagged `recovered` and carry an empty `worker_id`
/// (unknown) and `Completed` status (they are no longer active). Best-effort:
/// unreadable or malformed entries are skipped. Returns the number recovered.
pub async fn reconcile_sessions_from_captures(
    store: &dyn SessionStore,
    captures_dir: &Path,
) -> usize {
    let Ok(rd) = std::fs::read_dir(captures_dir) else {
        return 0; // captures dir absent → nothing to reconcile
    };

    let mut recovered = 0usize;
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let session_id = match dir.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if store.find_session(&session_id).await.is_some() {
            continue; // already indexed
        }
        if !dir.join("metadata.json").is_file() {
            // Check for tenant-partitioned capture directories: captures/<tenant_id>/<session_id>/
            if let Ok(sub_rd) = std::fs::read_dir(&dir) {
                for sub_entry in sub_rd.flatten() {
                    let sub_dir = sub_entry.path();
                    if !sub_dir.is_dir() {
                        continue;
                    }
                    let sub_session_id = match sub_dir.file_name().and_then(|n| n.to_str()) {
                        Some(s) => s.to_string(),
                        None => continue,
                    };
                    if store.find_session(&sub_session_id).await.is_some() {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(sub_dir.join("metadata.json")) else {
                        continue;
                    };
                    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                        continue;
                    };
                    store
                        .push_session(session_record_from_capture_meta(
                            &sub_session_id,
                            &meta,
                            &sub_dir,
                        ))
                        .await;
                    recovered += 1;
                }
            }
            continue;
        }
        let Ok(bytes) = std::fs::read(dir.join("metadata.json")) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        store
            .push_session(session_record_from_capture_meta(&session_id, &meta, &dir))
            .await;
        recovered += 1;
    }

    if recovered > 0 {
        tracing::info!(
            recovered,
            dir = %captures_dir.display(),
            "reconciled orphaned sessions from captures"
        );
    }
    recovered
}

/// Reconstruct a [`SessionRecord`] from a capture `metadata.json` value.
fn session_record_from_capture_meta(
    session_id: &str,
    meta: &serde_json::Value,
    dir: &Path,
) -> SessionRecord {
    let str_field = |key: &str| meta.get(key).and_then(|v| v.as_str()).map(String::from);
    let u32_field = |key: &str| {
        meta.get(key)
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as u32)
    };

    let started_at = meta
        .get("start_time")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc));

    let mut tags: Vec<String> = meta
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if !tags.iter().any(|t| t == "recovered") {
        tags.push("recovered".to_string());
    }

    let tenant_id = str_field("tenant_id").and_then(|t| voice_pipes_sdk::TenantId::new(t).ok());

    SessionRecord {
        id: session_id.to_string(),
        tenant_id,
        worker_id: String::new(),
        started_at,
        ended_at: None,
        status: SessionStatus::Completed,
        pipeline_name: str_field("pipeline_name"),
        pipeline_config_hash: str_field("pipeline_config_hash"),
        stt_provider: str_field("stt_provider"),
        tts_provider: str_field("tts_provider"),
        input_sample_rate: u32_field("input_sample_rate"),
        output_sample_rate: u32_field("output_sample_rate"),
        tags,
        turn_count: count_resolved_turns(dir),
        peak_rss_bytes: None,
        container_peak_memory_bytes: None,
        avg_turn_cpu_ms: None,
    }
}

/// Count completed turns in a capture dir by its `*_turn_resolved.json` files.
fn count_resolved_turns(dir: &Path) -> u32 {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with("_turn_resolved.json"))
            })
            .count() as u32
    })
}

#[cfg(test)]
#[path = "tests/store.rs"]
mod tests;
