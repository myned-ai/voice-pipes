//! `voice-pipes healthz`: probe a health endpoint and exit by its answer.
//!
//! The container images carry no HTTP client of their own, so their
//! `HEALTHCHECK` runs the binary they already ship. One image serves the
//! worker and the orchestrator on different ports; the URL says which.

use std::time::Duration;

use anyhow::Context as _;

/// The URL probed when none is given: a worker's transport liveness route.
pub const DEFAULT_HEALTHZ_URL: &str = "http://127.0.0.1:8800/healthz";

/// GET `url` within `timeout`.
///
/// # Errors
/// When the request fails or the answer is not a success status.
pub async fn run_healthz(url: &str, timeout: Duration) -> Result<(), anyhow::Error> {
    let client = voice_pipes_net::http_client(
        &voice_pipes_net::HttpOptions::request(timeout).connect_timeout(timeout),
    )
    .context("building the HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("{url} did not answer"))?;
    let status = response.status();
    anyhow::ensure!(status.is_success(), "{url} answered {status}");
    Ok(())
}

#[cfg(test)]
#[path = "tests/healthz.rs"]
mod tests;
