//! Where a credential may be sent in plain text.

use std::net::IpAddr;

/// The provider option an operator sets to send a credential over plain
/// `http://`/`ws://` to a host this module does not consider local.
pub use voice_pipes_sdk::options::ALLOW_INSECURE_OPTION;

/// Why a credential may not go to an endpoint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    /// Not a URL at all.
    #[error("endpoint is not a valid URL")]
    Invalid,
    /// A scheme no provider speaks.
    #[error("endpoint scheme `{0}` is not http(s) or ws(s)")]
    UnsupportedScheme(String),
    /// A credential would cross the network in clear text.
    #[error(
        "endpoint host `{0}` would receive the API key over plain text; use https/wss, or set \
         `allow_insecure_endpoint = true` if this network is trusted"
    )]
    Plaintext(String),
}

/// Check that `url` may receive a credential.
///
/// `https`/`wss` always may. `http`/`ws` may when `sends_credential` is
/// false, when the host is local (loopback, a private or link-local address,
/// a single-label name such as a container or LAN host, e.g.
/// `http://vibevoice:8091`, or a private-use name such as
/// `host.docker.internal`), or when the operator set `allow_plaintext`.
/// The URL itself never appears in the error: some carry a key in their
/// query string.
pub fn check_credential_endpoint(
    url: &str,
    sends_credential: bool,
    allow_plaintext: bool,
) -> Result<(), EndpointError> {
    let parsed = reqwest::Url::parse(url).map_err(|_unparseable| EndpointError::Invalid)?;
    match parsed.scheme() {
        "https" | "wss" => return Ok(()),
        "http" | "ws" => {}
        other => return Err(EndpointError::UnsupportedScheme(other.to_string())),
    }
    if !sends_credential || allow_plaintext {
        return Ok(());
    }
    let host = parsed.host_str().ok_or(EndpointError::Invalid)?;
    if is_local_host(host) {
        Ok(())
    } else {
        Err(EndpointError::Plaintext(host.to_string()))
    }
}

/// A host whose traffic stays on this machine or the local network.
fn is_local_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => {
                let first = v6.segments()[0];
                v6.is_loopback()
                    || (first & 0xfe00) == 0xfc00 // unique local, fc00::/7
                    || (first & 0xffc0) == 0xfe80 // link local, fe80::/10
            }
        };
    }
    let name = bare.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || !name.contains('.') || PRIVATE_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Name suffixes that never resolve on the public internet: `.localhost`
/// (RFC 6761), `.local` (mDNS, RFC 6762), `.home.arpa` (RFC 8375),
/// `.internal` (reserved for private use; Docker Desktop's
/// `host.docker.internal`) and the common `.lan`.
const PRIVATE_SUFFIXES: &[&str] = &[".localhost", ".local", ".home.arpa", ".internal", ".lan"];

/// For a provider's `validate`: a config error on `field` when `endpoint`
/// would receive a credential in plain text on a host that is not local,
/// unless the provider's `options` set `allow_insecure_endpoint = true`.
pub fn credential_endpoint_error<S: std::hash::BuildHasher>(
    field: &str,
    endpoint: &str,
    sends_credential: bool,
    options: &std::collections::HashMap<String, serde_json::Value, S>,
) -> Option<voice_pipes_sdk::ConfigError> {
    let allow = options
        .get(ALLOW_INSECURE_OPTION)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    check_credential_endpoint(endpoint, sends_credential, allow)
        .err()
        .map(|e| voice_pipes_sdk::ConfigError::new(field.to_string(), e.to_string()))
}

#[cfg(test)]
#[path = "tests/endpoint.rs"]
mod tests;
