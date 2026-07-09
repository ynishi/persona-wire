//! Indirect authentication reference layer for Adapter fetches
//! (`application::auth`).
//!
//! ## Why indirection
//!
//! A bundle's `[[wirings]]` entry (and the adapter `source_uri` it carries)
//! is persisted as plain-text `Node.metadata` — it must never hold a secret
//! value. Instead, wiring / bundle authors write a **credential reference
//! key** (`service_key`), and secret resolution happens later, at fetch
//! time, through the existing [`persona-wire-credentials`] provider chain
//! (env var → OS keyring). This module defines the domain-facing vocabulary
//! for that reference:
//!
//! ```text
//! bundle TOML / source_uri            SQLite (plain text OK: key names only)
//!   wiring: source_uri = "github://owner/repo"
//!           auth = "github-alt"   ← reference key only, never a secret
//!                     │
//!                     ▼
//!   AuthSpec { service_key, method: AuthMethod }
//!                     │
//!                     │ AuthResolver::resolve
//!                     ▼
//!   AuthMaterial::Bearer(SecretString)
//!                     │
//!   exposed only at the transport boundary (e.g.
//!   `persona_wire_transport_http::HttpClient::with_bearer`)
//! ```
//!
//! [`AuthResolver`] is implemented by `persona-wire-credentials`'s
//! `CredentialsAuthResolver` (wraps `Credentials::default_chain()`), so this
//! crate stays free of any concrete credential-backend dependency — it only
//! owns the shape of the reference, not how it resolves.
//!
//! ## Phase 1 scope
//!
//! Only [`AuthMethod::Bearer`] ships in Phase 1. `AuthMethod` and
//! [`AuthMaterial`] are both `#[non_exhaustive]` so that later phases
//! (`AtprotoSession` / `OAuth2` / token refresh) can add variants without a
//! breaking change; any `match` on either type from outside this crate must
//! carry a wildcard arm.

use secrecy::SecretString;

use crate::domain::error::WireResult;

/// Authentication method carried by an [`AuthSpec`].
///
/// Phase 1 ships only [`AuthMethod::Bearer`]; `#[non_exhaustive]` reserves
/// the enum for later phases (`AtprotoSession` / `OAuth2` / refresh)
/// without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthMethod {
    /// `Authorization: Bearer <token>` — the only method Phase 1 supports.
    #[default]
    Bearer,
}

/// A wiring entry's authentication reference — **never** the secret itself.
///
/// `service_key` is the vocabulary already used by
/// `persona_wire_credentials::Credentials::get(service)` (e.g. `"github"`,
/// or a custom key set via the adapter `?auth=<service_key>` URI query
/// param convention documented in `infrastructure::adapter`'s "External
/// service integration policy"). Persisting an `AuthSpec` (or its
/// `service_key` alone, as bundle/node metadata does) is safe — the secret
/// value never appears here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthSpec {
    /// Credential reference key — looked up via
    /// `persona_wire_credentials::Credentials::get(&service_key)`, never a
    /// secret value itself.
    pub service_key: String,
    /// Authentication method. Defaults to [`AuthMethod::Bearer`] when
    /// omitted (e.g. in TOML / JSON where only `service_key` is given).
    #[serde(default)]
    pub method: AuthMethod,
}

impl AuthSpec {
    /// Builds a Bearer [`AuthSpec`] for `service_key` — the common-case
    /// constructor for Phase 1 (the only method currently supported).
    pub fn bearer(service_key: impl Into<String>) -> Self {
        Self {
            service_key: service_key.into(),
            method: AuthMethod::Bearer,
        }
    }
}

/// Resolved authentication material — the actual secret, held as a
/// [`secrecy::SecretString`] so it is never exposed via `Debug` / logging.
///
/// `#[derive(Debug)]` here is safe: it rides `secrecy::SecretString`'s own
/// `Debug` impl, which always prints a redacted placeholder
/// (`SecretBox<str>([REDACTED])`) rather than the wrapped value — the same
/// property `persona_wire_transport_http::HttpClient` relies on for its
/// `bearer` field.
///
/// `#[non_exhaustive]` for the same forward-compat reason as
/// [`AuthMethod`].
#[derive(Debug)]
#[non_exhaustive]
pub enum AuthMaterial {
    /// Resolved Bearer token, exposed only at the transport boundary (e.g.
    /// `persona_wire_transport_http::HttpClient::with_bearer` /
    /// `reqwest::RequestBuilder::bearer_auth`).
    Bearer(SecretString),
}

/// Resolves an [`AuthSpec`] into concrete [`AuthMaterial`], or `None` when
/// no credential is configured for `spec.service_key`.
///
/// Implemented by `persona-wire-credentials`'s `CredentialsAuthResolver`
/// (wraps `Credentials::default_chain()`); kept as a trait here so
/// `persona-wire-core` stays free of any concrete credential-backend
/// dependency. `resolve` is sync — mirrors
/// `persona_wire_credentials::Credentials::get`, which is itself sync (the
/// env / keyring provider chain performs no async I/O).
///
/// Error semantics mirror `Credentials::get`: `Ok(None)` means "no
/// credential configured for this service" (not an error); `Err`
/// propagates a real backend failure (e.g. keychain access denied) —
/// fail loud, never silently swallowed into `Ok(None)`.
pub trait AuthResolver: Send + Sync {
    /// Resolve `spec` into [`AuthMaterial`], or `Ok(None)` when unconfigured.
    fn resolve(&self, spec: &AuthSpec) -> WireResult<Option<AuthMaterial>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    // ---- AuthMethod ----

    #[test]
    fn auth_method_default_is_bearer() {
        assert_eq!(AuthMethod::default(), AuthMethod::Bearer);
    }

    #[test]
    fn auth_method_serde_roundtrip() {
        let json = serde_json::to_string(&AuthMethod::Bearer).unwrap();
        assert_eq!(json, "\"bearer\"", "snake_case rename");
        let back: AuthMethod = serde_json::from_str(&json).unwrap();
        assert_eq!(back, AuthMethod::Bearer);
    }

    // ---- AuthSpec ----

    #[test]
    fn auth_spec_bearer_constructor_sets_method() {
        let spec = AuthSpec::bearer("github-alt");
        assert_eq!(spec.service_key, "github-alt");
        assert_eq!(spec.method, AuthMethod::Bearer);
    }

    #[test]
    fn auth_spec_serde_roundtrip() {
        let spec = AuthSpec::bearer("notion");
        let json = serde_json::to_string(&spec).unwrap();
        let back: AuthSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn auth_spec_method_defaults_when_omitted_from_json() {
        // service_key only, no `method` key — must default to Bearer rather
        // than fail to deserialize (bundle / metadata authors may omit it).
        let json = r#"{"service_key": "slack"}"#;
        let spec: AuthSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.service_key, "slack");
        assert_eq!(spec.method, AuthMethod::Bearer);
    }

    // ---- AuthMaterial ----

    #[test]
    fn auth_material_debug_does_not_leak_secret() {
        let material = AuthMaterial::Bearer(SecretString::from("super-secret-token".to_string()));
        let debug_str = format!("{material:?}");
        assert!(
            !debug_str.contains("super-secret-token"),
            "Debug output must not contain the raw secret, got: {debug_str}"
        );
    }

    #[test]
    fn auth_material_bearer_exposes_secret_only_via_expose_secret() {
        let material = AuthMaterial::Bearer(SecretString::from("tok-123".to_string()));
        let AuthMaterial::Bearer(secret) = material;
        assert_eq!(secret.expose_secret(), "tok-123");
    }
}
