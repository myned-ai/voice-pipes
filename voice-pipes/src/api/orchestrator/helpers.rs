use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::api::types::{
    ApiResponse, AuditFileItem, AuditTapItem, DiffEntry, HistogramSummary, OrchestratorState,
    WorkerStatus,
};
use crate::audit::store::{AuditStore, StoredBlob};

// ── Helpers ─────────────────────────────────────────────────

pub fn worker_counts(state: &OrchestratorState) -> (usize, usize, usize, usize) {
    let mut total = 0usize;
    let mut active = 0usize;
    let mut draining = 0usize;
    let mut dead = 0usize;
    for entry in state.workers.iter() {
        total += 1;
        match entry.value().status {
            WorkerStatus::Active => active += 1,
            WorkerStatus::Draining => draining += 1,
            WorkerStatus::Dead => dead += 1,
        }
    }
    (total, active, draining, dead)
}

pub fn compute_histogram(values: &[u64]) -> HistogramSummary {
    if values.is_empty() {
        return HistogramSummary::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let len = sorted.len();
    let sum: u64 = sorted.iter().sum();

    HistogramSummary {
        p50: sorted[len * 50 / 100],
        p90: sorted[len * 90 / 100],
        p99: sorted[(len * 99 / 100).min(len - 1)],
        mean: sum / len as u64,
        min: sorted[0],
        max: sorted[len - 1],
    }
}

pub fn parse_audit_blobs(
    blobs: &[StoredBlob],
    session_filter: Option<&str>,
    type_filter: Option<&str>,
) -> Vec<AuditFileItem> {
    let mut items = Vec::new();
    for blob in blobs {
        let name = &blob.key;
        let ext = name.rsplit('.').next().unwrap_or("");

        if let Some(tf) = type_filter
            && ext != tf
        {
            continue;
        }

        // Parse session_id from key: {timestamp}_{session_id}.{ext}
        let session_id = name.split('_').nth(1).map(|s| {
            s.rsplit_once('.')
                .map_or_else(|| s.to_string(), |(id, _)| id.to_string())
        });

        if let Some(sf) = session_filter
            && session_id.as_deref() != Some(sf)
        {
            continue;
        }

        items.push(AuditFileItem {
            filename: name.clone(),
            size_bytes: blob.size_bytes,
            file_type: ext.to_string(),
            session_id,
            worker_id: None,
            created_at: blob.created_at,
        });
    }
    items.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    items
}

pub fn parse_tap_blobs(
    blobs: &[StoredBlob],
    label_filter: Option<&str>,
    tag_filter: Option<&str>,
) -> Vec<AuditTapItem> {
    let mut items = Vec::new();
    for blob in blobs {
        let name = &blob.key;

        // Parse: {seq:04}_{label}_{tag}_{rate}hz.wav
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
        let parts: Vec<&str> = stem.split('_').collect();
        if parts.len() < 4 {
            continue;
        }

        let sequence: u64 = parts[0].parse().unwrap_or(0);
        let label = parts[1].to_string();
        let tag = parts[2].to_string();
        let rate_str = parts[3].trim_end_matches("hz");
        let sample_rate: u32 = rate_str.parse().unwrap_or(0);

        if let Some(lf) = label_filter
            && label != lf
        {
            continue;
        }
        if let Some(tf) = tag_filter
            && tag != tf
        {
            continue;
        }

        items.push(AuditTapItem {
            filename: name.clone(),
            size_bytes: blob.size_bytes,
            label,
            tag,
            sample_rate,
            sequence,
            worker_id: None,
            created_at: blob.created_at,
        });
    }
    items.sort_by_key(|b| std::cmp::Reverse(b.sequence));
    items
}

pub async fn serve_blob(store: &dyn AuditStore, key: &str) -> Response {
    if key.contains("..") {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::bad_request("Invalid filename")),
        )
            .into_response();
    }

    // Streamed from the store rather than loaded whole: a capture WAV
    // can be tens of megabytes and this runs per admin-UI request.
    match store.open(key).await {
        Ok(Some(reader)) => {
            let is_wav = std::path::Path::new(key)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
            let content_type = if is_wav {
                "audio/wav"
            } else {
                "text/plain; charset=utf-8"
            };
            (
                StatusCode::OK,
                [
                    ("content-type", content_type),
                    ("content-disposition", "inline"),
                ],
                axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(reader)),
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "File not found: {key}"
            ))),
        )
            .into_response(),
        Err(e) => {
            // The store refuses malformed keys with a validation error;
            // that is the caller's mistake, not a server failure.
            let msg = e.to_string();
            if msg.contains("invalid blob key") {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<()>::bad_request("Invalid filename")),
                )
                    .into_response();
            }
            tracing::warn!(key, error = %msg, "blob read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::internal_error("blob read failed")),
            )
                .into_response()
        }
    }
}

/// Recursively compare two JSON values, collecting all leaf differences.
pub fn collect_diffs(
    prefix: &str,
    a: &serde_json::Value,
    b: &serde_json::Value,
    out: &mut Vec<DiffEntry>,
) {
    match (a, b) {
        (serde_json::Value::Object(ma), serde_json::Value::Object(mb)) => {
            let all_keys: std::collections::BTreeSet<&str> =
                ma.keys().chain(mb.keys()).map(String::as_str).collect();
            for key in all_keys {
                let path = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                match (ma.get(key), mb.get(key)) {
                    (Some(va), Some(vb)) => collect_diffs(&path, va, vb, out),
                    (Some(va), None) => out.push(DiffEntry {
                        path,
                        a: Some(va.clone()),
                        b: None,
                    }),
                    (None, Some(vb)) => out.push(DiffEntry {
                        path,
                        a: None,
                        b: Some(vb.clone()),
                    }),
                    (None, None) => {}
                }
            }
        }
        _ if a != b => {
            out.push(DiffEntry {
                path: prefix.to_string(),
                a: Some(a.clone()),
                b: Some(b.clone()),
            });
        }
        _ => {} // equal
    }
}
