//! Provider-chain credential resolution for persona-wire Adapters
//! (`persona-wire-credentials`).
//!
//! ## Architecture
//!
//! ```text
//! Credentials::default_chain()
//!        │
//!        ├─ EnvTokenProvider     (checked first)
//!        └─ KeyringTokenProvider (checked second, OS keychain)
//!                │
//!                ▼
//!        Option<secrecy::SecretString>
//! ```
//!
//! [`Credentials`] holds an ordered list of [`TokenProvider`] impls and
//! queries them in order, returning the first `Some(token)`. Any adapter
//! needing an external-service API token asks `Credentials::get(service)`
//! rather than reading `std::env` or the OS keychain directly, so the
//! precedence and fail-loud behavior below is uniform across every adapter.
//!
//! ## Precedence
//!
//! For a given `service` (e.g. `"github"`), [`EnvTokenProvider`] checks, in
//! order:
//!
//! 1. `PERSONA_WIRE_TOKEN_<SERVICE>` (`service` upper-cased, `-` → `_`).
//! 2. The conventional alias env var in [`ALIAS_ENV_VARS`], if `service` has
//!    one (e.g. `github` → `GITHUB_TOKEN`).
//!
//! An env var set to the empty string is treated as absent (`None`), not as
//! an empty token. [`KeyringTokenProvider`] is checked only if no env
//! provider (in [`Credentials::default_chain`], `EnvTokenProvider`) supplied
//! a token.
//!
//! ## Security notes
//!
//! - Every token is [`secrecy::SecretString`] end to end; call
//!   `secrecy::ExposeSecret::expose_secret` only at the point of use (e.g.
//!   building an `Authorization` header), never for logging or `Debug`.
//! - Never place a token in a `source_uri` or any other logged/printed
//!   value.
//! - **Fail loud**: a provider-level error (e.g. keychain access denied)
//!   propagates as `Err`, it is never silently swallowed into `Ok(None)`.
//!   Only "this provider has no entry for this service" is `Ok(None)`.

#![warn(missing_docs)]

use persona_wire_core::{WireError, WireResult};
use secrecy::SecretString;

/// Conventional env var aliases for well-known services (`service` name →
/// env var name), consulted by [`EnvTokenProvider`] after the
/// `PERSONA_WIRE_TOKEN_<SERVICE>` lookup.
pub const ALIAS_ENV_VARS: &[(&str, &str)] = &[
    ("github", "GITHUB_TOKEN"),
    ("todoist", "TODOIST_API_TOKEN"),
    ("notion", "NOTION_TOKEN"),
    ("slack", "SLACK_BOT_TOKEN"),
];

/// A single credential source consulted by [`Credentials`].
pub trait TokenProvider: Send + Sync {
    /// Short identifier for this provider (e.g. `"env"` / `"keyring"`),
    /// used by [`Credentials::resolve_source`] without exposing the token
    /// itself.
    fn name(&self) -> &'static str;

    /// Look up a token for `service`. `Ok(None)` means "this provider has no
    /// entry for this service" (try the next provider); `Err` means a real
    /// failure in this provider and must not be swallowed (fail loud).
    fn get(&self, service: &str) -> WireResult<Option<SecretString>>;
}

/// Looks up tokens from process environment variables.
///
/// See the module-level "Precedence" section for the exact lookup order.
pub struct EnvTokenProvider;

impl EnvTokenProvider {
    /// The primary (non-alias) env var name for `service`
    /// (`PERSONA_WIRE_TOKEN_<SERVICE>`, upper-cased with `-` → `_`).
    fn primary_var_name(service: &str) -> String {
        let normalized = service.to_uppercase().replace('-', "_");
        format!("PERSONA_WIRE_TOKEN_{normalized}")
    }

    /// Reads `var`, returning `Some` only for a present *and* non-empty
    /// value (an empty env var is treated as absent).
    fn read_non_empty(var: &str) -> Option<SecretString> {
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => Some(SecretString::from(v)),
            _ => None,
        }
    }
}

impl TokenProvider for EnvTokenProvider {
    fn name(&self) -> &'static str {
        "env"
    }

    fn get(&self, service: &str) -> WireResult<Option<SecretString>> {
        let primary = Self::primary_var_name(service);
        if let Some(token) = Self::read_non_empty(&primary) {
            return Ok(Some(token));
        }
        if let Some((_, alias_var)) = ALIAS_ENV_VARS.iter().find(|(svc, _)| *svc == service) {
            if let Some(token) = Self::read_non_empty(alias_var) {
                return Ok(Some(token));
            }
        }
        Ok(None)
    }
}

/// Looks up (and manages) tokens in the OS keychain, under the
/// `persona-wire` service namespace and `service` as the account/username.
///
/// Backed by the `keyring` crate (Keychain on macOS, Credential Manager on
/// Windows, Secret Service on Linux).
pub struct KeyringTokenProvider;

impl KeyringTokenProvider {
    fn entry(service: &str) -> WireResult<keyring::Entry> {
        keyring::Entry::new("persona-wire", service).map_err(|e| {
            WireError::Storage(format!(
                "credentials: keyring entry build for '{service}': {e}"
            ))
        })
    }

    /// Store `token` in the OS keychain for `service`, overwriting any
    /// existing entry.
    pub fn set(&self, service: &str, token: &str) -> WireResult<()> {
        Self::entry(service)?.set_password(token).map_err(|e| {
            WireError::Storage(format!("credentials: keyring set for '{service}': {e}"))
        })
    }

    /// Delete the OS keychain entry for `service`, if any. Idempotent — a
    /// missing entry is `Ok(())`, not an error.
    pub fn delete(&self, service: &str) -> WireResult<()> {
        match Self::entry(service)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(WireError::Storage(format!(
                "credentials: keyring delete for '{service}': {e}"
            ))),
        }
    }
}

impl TokenProvider for KeyringTokenProvider {
    fn name(&self) -> &'static str {
        "keyring"
    }

    fn get(&self, service: &str) -> WireResult<Option<SecretString>> {
        match Self::entry(service)?.get_password() {
            Ok(pw) => Ok(Some(SecretString::from(pw))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(WireError::Storage(format!(
                "credentials: keyring lookup for '{service}': {e}"
            ))),
        }
    }
}

/// Ordered provider chain for token resolution. See module docs.
pub struct Credentials {
    providers: Vec<Box<dyn TokenProvider>>,
}

impl Credentials {
    /// The default provider chain: [`EnvTokenProvider`] then
    /// [`KeyringTokenProvider`].
    pub fn default_chain() -> Self {
        Self::with_providers(vec![
            Box::new(EnvTokenProvider),
            Box::new(KeyringTokenProvider),
        ])
    }

    /// A custom provider chain, consulted in the given order.
    pub fn with_providers(providers: Vec<Box<dyn TokenProvider>>) -> Self {
        Self { providers }
    }

    /// Resolve a token for `service` by consulting each provider in chain
    /// order, returning the first `Some`. A provider-level `Err` propagates
    /// immediately (fail loud — never silently skipped).
    pub fn get(&self, service: &str) -> WireResult<Option<SecretString>> {
        for provider in &self.providers {
            if let Some(token) = provider.get(service)? {
                return Ok(Some(token));
            }
        }
        Ok(None)
    }

    /// Like [`Credentials::get`], but returns only the supplying provider's
    /// [`TokenProvider::name`] instead of the token itself (for status /
    /// diagnostics UIs that must not expose the token value).
    pub fn resolve_source(&self, service: &str) -> WireResult<Option<&'static str>> {
        for provider in &self.providers {
            if provider.get(service)?.is_some() {
                return Ok(Some(provider.name()));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::sync::atomic::{AtomicBool, Ordering};

    // ---- EnvTokenProvider ----

    #[test]
    fn env_provider_reads_primary_var() {
        let service = "test-service-primary-alpha";
        let var = EnvTokenProvider::primary_var_name(service);
        std::env::set_var(&var, "primary-tok");
        let got = EnvTokenProvider.get(service).unwrap();
        std::env::remove_var(&var);
        assert_eq!(got.unwrap().expose_secret(), "primary-tok");
    }

    // Both cases below share the `github` / `GITHUB_TOKEN` env vars, which are
    // process-global state. `cargo test` runs tests in parallel threads, so
    // these must live in a single test function (sequential within one test)
    // rather than two separate `#[test]` fns — otherwise they race on the
    // same env vars and flake.
    #[test]
    fn env_provider_alias_fallback_and_primary_precedence() {
        std::env::remove_var("PERSONA_WIRE_TOKEN_GITHUB");
        std::env::remove_var("GITHUB_TOKEN");

        // No primary var set → falls back to the alias env var.
        std::env::set_var("GITHUB_TOKEN", "alias-tok");
        let got = EnvTokenProvider.get("github").unwrap();
        assert_eq!(got.unwrap().expose_secret(), "alias-tok");

        // Primary var set alongside the alias → primary wins.
        std::env::set_var("PERSONA_WIRE_TOKEN_GITHUB", "primary-wins");
        let got = EnvTokenProvider.get("github").unwrap();
        assert_eq!(got.unwrap().expose_secret(), "primary-wins");

        std::env::remove_var("PERSONA_WIRE_TOKEN_GITHUB");
        std::env::remove_var("GITHUB_TOKEN");
    }

    #[test]
    fn env_provider_empty_value_treated_as_absent() {
        let service = "test-service-empty-beta";
        let var = EnvTokenProvider::primary_var_name(service);
        std::env::set_var(&var, "");
        let got = EnvTokenProvider.get(service).unwrap();
        std::env::remove_var(&var);
        assert!(got.is_none());
    }

    #[test]
    fn env_provider_missing_returns_none() {
        let got = EnvTokenProvider
            .get("test-service-never-set-gamma")
            .unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn primary_var_name_normalizes_hyphen_and_case() {
        assert_eq!(
            EnvTokenProvider::primary_var_name("my-service"),
            "PERSONA_WIRE_TOKEN_MY_SERVICE"
        );
    }

    // ---- Credentials chain logic (mock providers, no real env/keyring) ----

    struct MockProvider {
        name: &'static str,
        result: Option<&'static str>,
        called: AtomicBool,
    }

    impl TokenProvider for MockProvider {
        fn name(&self) -> &'static str {
            self.name
        }
        fn get(&self, _service: &str) -> WireResult<Option<SecretString>> {
            self.called.store(true, Ordering::SeqCst);
            Ok(self.result.map(|s| SecretString::from(s.to_string())))
        }
    }

    struct FailingProvider;
    impl TokenProvider for FailingProvider {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn get(&self, service: &str) -> WireResult<Option<SecretString>> {
            Err(WireError::Storage(format!("boom for {service}")))
        }
    }

    #[test]
    fn chain_returns_first_some() {
        let creds = Credentials::with_providers(vec![
            Box::new(MockProvider {
                name: "first",
                result: Some("tok-a"),
                called: AtomicBool::new(false),
            }),
            Box::new(MockProvider {
                name: "second",
                result: Some("tok-b"),
                called: AtomicBool::new(false),
            }),
        ]);
        let got = creds.get("svc").unwrap();
        assert_eq!(got.unwrap().expose_secret(), "tok-a");
    }

    #[test]
    fn chain_falls_through_none_to_next_provider() {
        let creds = Credentials::with_providers(vec![
            Box::new(MockProvider {
                name: "first",
                result: None,
                called: AtomicBool::new(false),
            }),
            Box::new(MockProvider {
                name: "second",
                result: Some("tok-b"),
                called: AtomicBool::new(false),
            }),
        ]);
        let got = creds.get("svc").unwrap();
        assert_eq!(got.unwrap().expose_secret(), "tok-b");
    }

    #[test]
    fn chain_returns_none_when_all_providers_miss() {
        let creds = Credentials::with_providers(vec![Box::new(MockProvider {
            name: "only",
            result: None,
            called: AtomicBool::new(false),
        })]);
        assert!(creds.get("svc").unwrap().is_none());
    }

    #[test]
    fn chain_propagates_provider_error_fail_loud() {
        let creds = Credentials::with_providers(vec![
            Box::new(FailingProvider),
            Box::new(MockProvider {
                name: "never-reached",
                result: Some("tok-b"),
                called: AtomicBool::new(false),
            }),
        ]);
        let err = creds.get("svc").unwrap_err();
        assert!(format!("{err}").contains("boom for svc"));
    }

    #[test]
    fn resolve_source_returns_provider_name_not_token() {
        let creds = Credentials::with_providers(vec![Box::new(MockProvider {
            name: "second",
            result: Some("tok-b"),
            called: AtomicBool::new(false),
        })]);
        assert_eq!(creds.resolve_source("svc").unwrap(), Some("second"));
    }

    #[test]
    fn resolve_source_none_when_all_providers_miss() {
        let creds = Credentials::with_providers(vec![Box::new(MockProvider {
            name: "only",
            result: None,
            called: AtomicBool::new(false),
        })]);
        assert!(creds.resolve_source("svc").unwrap().is_none());
    }
}
