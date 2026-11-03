//! Credential resolution for the Google STT provider.
//!
//! One bearer token is fetched per upstream stream: a stream
//! lives at most 5 minutes while tokens live ~60, and `gcp_auth` caches the
//! token until it expires, so this is one cheap call per stream and no
//! background refresh machinery.
//!
//! Resolution order:
//! 1. `access_token` (config / `GOOGLE_OAUTH_ACCESS_TOKEN`) — used verbatim.
//! 2. `credentials_path` — a service-account JSON key.
//! 3. Application Default Credentials via [`gcp_auth::provider`]:
//!    `GOOGLE_APPLICATION_CREDENTIALS`, the gcloud ADC file, the GCE
//!    metadata server, then `gcloud auth print-access-token`.

use std::path::PathBuf;
use std::sync::Arc;

use gcp_auth::TokenProvider;
use tokio::sync::OnceCell;

use voice_pipes_sdk::stt::SttError;

/// OAuth scope that covers Speech-to-Text.
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Lazily-initialised credential source shared by every stream of a
/// provider instance.
pub struct AuthSource {
    static_token: Option<voice_pipes_sdk::secrets::Secret>,
    credentials_path: Option<PathBuf>,
    project_hint: Option<String>,
    provider: OnceCell<Arc<dyn TokenProvider>>,
}

impl std::fmt::Debug for AuthSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthSource")
            .field("static_token", &self.static_token.as_ref().map(|_| "***"))
            .field("credentials_path", &self.credentials_path)
            .field("project_hint", &self.project_hint)
            .field("initialised", &self.provider.initialized())
            .finish()
    }
}

impl AuthSource {
    pub fn new(
        static_token: Option<voice_pipes_sdk::secrets::Secret>,
        credentials_path: Option<PathBuf>,
        project_hint: Option<String>,
    ) -> Self {
        Self {
            static_token,
            credentials_path,
            project_hint,
            provider: OnceCell::new(),
        }
    }

    /// A source backed by a caller-supplied token provider; lets tests
    /// stand in a stalling or scripted credential without touching the
    /// real credential chain.
    #[cfg(test)]
    pub(crate) fn with_provider(provider: Arc<dyn TokenProvider>) -> Self {
        let cell = OnceCell::new();
        let _ = cell.set(provider);
        Self {
            static_token: None,
            credentials_path: None,
            project_hint: Some("proj".into()),
            provider: cell,
        }
    }

    async fn provider(&self) -> Result<&Arc<dyn TokenProvider>, SttError> {
        self.provider
            .get_or_try_init(|| async {
                match &self.credentials_path {
                    Some(path) => {
                        let sa = gcp_auth::CustomServiceAccount::from_file(path).map_err(|e| {
                            SttError::AuthError(format!(
                                "service-account key {} unusable: {e}",
                                path.display()
                            ))
                        })?;
                        Ok(Arc::new(sa) as Arc<dyn TokenProvider>)
                    }
                    None => gcp_auth::provider().await.map_err(|e| {
                        SttError::AuthError(format!(
                            "no Google credentials found ({e}); set `credentials_path` to a \
                             service-account key, or provide Application Default Credentials \
                             (GOOGLE_APPLICATION_CREDENTIALS / `gcloud auth application-default \
                             login`)"
                        ))
                    }),
                }
            })
            .await
    }

    /// A bearer token valid for at least the next few minutes.
    pub async fn bearer(&self) -> Result<voice_pipes_sdk::secrets::Secret, SttError> {
        if let Some(tok) = &self.static_token {
            return Ok(tok.clone());
        }
        let provider = self.provider().await?;
        let token = provider
            .token(&[SCOPE])
            .await
            .map_err(classify_gcp_auth_error)?;
        Ok(voice_pipes_sdk::secrets::Secret::from(token.as_str()))
    }

    /// The project used for the recognizer path and quota attribution:
    /// the configured hint, else whatever the credential reports.
    pub async fn project_id(&self) -> Result<String, SttError> {
        if let Some(p) = &self.project_hint {
            return Ok(p.clone());
        }
        if self.static_token.is_some() {
            return Err(SttError::ConfigError(format!(
                "`project_id` (or {}) is required when authenticating with a static access token",
                super::config::PROJECT_ENV_VAR
            )));
        }
        let provider = self.provider().await?;
        provider
            .project_id()
            .await
            .map(|p| p.to_string())
            .map_err(|e| {
                SttError::ConfigError(format!(
                    "credential does not carry a project id ({e}); set `project_id` or {}",
                    super::config::PROJECT_ENV_VAR
                ))
            })
    }
}

/// Token-fetch failures: network-shaped ones are transient (retried with
/// backoff), everything else is a permanent auth error.
fn classify_gcp_auth_error(e: gcp_auth::Error) -> SttError {
    match e {
        gcp_auth::Error::Http(..) | gcp_auth::Error::Io(..) => {
            SttError::NetworkError(format!("token fetch failed: {e}"))
        }
        other => SttError::AuthError(format!("token fetch failed: {other}")),
    }
}

#[cfg(test)]
#[path = "tests/auth.rs"]
mod tests;
