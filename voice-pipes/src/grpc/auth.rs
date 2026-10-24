//! Bearer-token auth for the gRPC surfaces.
//!
//! gRPC components are machine-to-machine infrastructure, so both sides
//! use the *internal* credential (`VP_INTERNAL_TOKEN` by default; the
//! component server reads the env-var name from its `[auth]` config).
//! Same runtime-flag semantics as the HTTP surfaces: no token = open
//! (dev mode), token set = enforced, `required = true` = startup error
//! when unset.

use std::sync::Arc;

use tonic::service::Interceptor;
use tonic::{Request, Status};

use crate::api::auth::SurfaceAuth;

/// Server-side interceptor: rejects calls whose `authorization`
/// metadata does not carry the expected bearer token. Pass-through when
/// the surface is open.
#[derive(Clone, Debug)]
pub struct ServerBearerCheck {
    auth: SurfaceAuth,
}

impl ServerBearerCheck {
    pub fn new(auth: SurfaceAuth) -> Self {
        Self { auth }
    }
}

impl Interceptor for ServerBearerCheck {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> {
        let presented = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        if self.auth.check(presented) {
            Ok(request)
        } else {
            Err(Status::unauthenticated("missing or invalid token"))
        }
    }
}

/// Client-side interceptor: attaches `authorization: Bearer <token>` to
/// every outbound call. No-op when the credential is unset, so local
/// split-mode setups keep working with zero configuration.
#[derive(Clone)]
pub struct ClientBearer {
    token: Option<Arc<str>>,
}

impl std::fmt::Debug for ClientBearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the token value.
        f.debug_struct("ClientBearer")
            .field("enabled", &self.token.is_some())
            .finish()
    }
}

impl ClientBearer {
    /// Read the standard internal-token env var (`VP_INTERNAL_TOKEN`).
    /// gRPC clients are built from provider factories that don't carry
    /// the `[auth]` section, so the env-var name is not remappable on
    /// the client side.
    pub fn from_env() -> Self {
        let token = std::env::var(crate::api::auth::AuthSettings::default().internal_token_env)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(Arc::from);
        Self { token }
    }
}

impl Interceptor for ClientBearer {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = &self.token
            && let Ok(value) = format!("Bearer {token}").parse()
        {
            request.metadata_mut().insert("authorization", value);
        }
        Ok(request)
    }
}

/// The channel type all internal gRPC clients use: a plain tonic
/// [`Channel`](tonic::transport::Channel) wrapped with [`ClientBearer`].
pub type AuthedChannel =
    tonic::service::interceptor::InterceptedService<tonic::transport::Channel, ClientBearer>;

/// Wrap a connected channel with the standard client credential.
pub fn authed_channel(channel: tonic::transport::Channel) -> AuthedChannel {
    tonic::service::interceptor::InterceptedService::new(channel, ClientBearer::from_env())
}

#[cfg(test)]
#[path = "tests/auth.rs"]
mod tests;
