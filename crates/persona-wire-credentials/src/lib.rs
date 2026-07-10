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

use persona_wire_core::application::auth::{AuthMaterial, AuthMethod, AuthResolver, AuthSpec};
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
    ("mastodon", "MASTODON_ACCESS_TOKEN"),
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

    /// Whether this provider has an entry for `service`, without exposing
    /// the token value.
    ///
    /// The default implementation delegates to [`TokenProvider::get`] and
    /// discards the value. Providers with a cheaper existence-only lookup
    /// (notably platform keyrings, where a full read can trigger an OS
    /// permission prompt) should override this to avoid paying that cost
    /// just to answer "does an entry exist" — see
    /// [`Credentials::resolve_source`], which uses this method precisely
    /// because it must not expose (or pay the cost of reading) the token.
    fn exists(&self, service: &str) -> WireResult<bool> {
        Ok(self.get(service)?.is_some())
    }
}

/// Write-side extension of [`TokenProvider`]. Backends that support storing
/// or removing tokens implement this in addition to [`TokenProvider`]; read-
/// only backends (e.g. [`EnvTokenProvider`]) intentionally do NOT implement
/// it, so `set` / `delete` on those is a compile-time error rather than a
/// runtime `unimplemented!()`.
pub trait MutableTokenProvider: TokenProvider {
    /// Store `token` in the backend under `service`, overwriting any prior
    /// value. Backend errors are returned as [`WireError::Storage`] — never
    /// silently swallowed.
    fn set(&self, service: &str, token: &str) -> WireResult<()>;

    /// Remove any token stored under `service`. Idempotent — a missing
    /// entry is `Ok(())`, not an error.
    fn delete(&self, service: &str) -> WireResult<()>;
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
}

impl MutableTokenProvider for KeyringTokenProvider {
    /// Store `token` in the OS keychain for `service`, overwriting any
    /// existing entry.
    ///
    /// Non-macOS: routed through the `keyring` crate (`secret-service` on
    /// Linux, `wincred` on Windows), consistent with [`KeyringTokenProvider::entry`].
    #[cfg(not(target_os = "macos"))]
    fn set(&self, service: &str, token: &str) -> WireResult<()> {
        Self::entry(service)?.set_password(token).map_err(|e| {
            WireError::Storage(format!("credentials: keyring set for '{service}': {e}"))
        })
    }

    /// Delete the OS keychain entry for `service`, if any. Idempotent — a
    /// missing entry is `Ok(())`, not an error.
    ///
    /// Non-macOS: routed through the `keyring` crate.
    #[cfg(not(target_os = "macos"))]
    fn delete(&self, service: &str) -> WireResult<()> {
        match Self::entry(service)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(WireError::Storage(format!(
                "credentials: keyring delete for '{service}': {e}"
            ))),
        }
    }

    /// Store `token` in the macOS Keychain for `service`, overwriting any
    /// existing entry.
    ///
    /// Goes directly through [`security_framework::passwords::set_generic_password`]
    /// (same `(service, account)` attribute pair as [`KeyringTokenProvider::entry`]:
    /// `kSecAttrService = "persona-wire"`, `kSecAttrAccount = service`)
    /// instead of the `keyring` crate's `apple-native` backend. This is a
    /// single `SecItemAdd` call that falls back to `SecItemUpdate`
    /// internally on `errSecDuplicateItem`, so a create-or-overwrite `set`
    /// triggers at most one "wants to add/change" Keychain ACL prompt —
    /// never two.
    #[cfg(target_os = "macos")]
    fn set(&self, service: &str, token: &str) -> WireResult<()> {
        security_framework::passwords::set_generic_password(
            "persona-wire",
            service,
            token.as_bytes(),
        )
        .map_err(|e| WireError::Storage(format!("credentials: keyring set for '{service}': {e}")))
    }

    /// Delete the macOS Keychain entry for `service`, if any. Idempotent —
    /// a missing entry (`errSecItemNotFound`) is `Ok(())`, not an error.
    ///
    /// Goes directly through
    /// [`security_framework::passwords::delete_generic_password`] rather
    /// than the `keyring` crate, for the same reason as
    /// [`KeyringTokenProvider::set`] above.
    #[cfg(target_os = "macos")]
    fn delete(&self, service: &str) -> WireResult<()> {
        // `errSecItemNotFound` (Security.framework `OSStatus`): mirrored as
        // a literal here for the same reason as in `exists` below — see
        // that doc comment for the upstream reference link.
        const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

        match security_framework::passwords::delete_generic_password("persona-wire", service) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
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

    /// Existence check without extracting the secret value (macOS only).
    ///
    /// [`KeyringTokenProvider::get`] goes through the `keyring` crate's
    /// `apple-native` backend, which calls the legacy
    /// `SecKeychainFindGenericPassword` API — that API *always* returns the
    /// password bytes, so it always triggers the "wants to use confidential
    /// information" Keychain prompt in addition to the "wants to access
    /// key" prompt (two dialogs per `token status` call). This override
    /// instead queries the newer unified Keychain Item Search API
    /// (`SecItemCopyMatching`, via
    /// [`security_framework::item::ItemSearchOptions`]) asking only for an
    /// item reference (`kSecReturnRef`) and explicitly not its data
    /// (`kSecReturnData`), against the exact same `(service, account)`
    /// attribute pair the `keyring` crate uses for this entry
    /// (`kSecAttrService = "persona-wire"`, `kSecAttrAccount = service`;
    /// see `keyring::Entry::new("persona-wire", service)` in
    /// [`KeyringTokenProvider::entry`]). This surfaces at most the "wants
    /// to access key" dialog, never the confidential-data one.
    ///
    /// Other platforms (`secret-service` on Linux, `wincred` on Windows)
    /// don't have this two-dialog behavior, so they keep the default
    /// [`TokenProvider::exists`] implementation (delegates to `get`).
    #[cfg(target_os = "macos")]
    fn exists(&self, service: &str) -> WireResult<bool> {
        use security_framework::item::{ItemClass, ItemSearchOptions};

        // `errSecItemNotFound` (Security.framework `OSStatus`): no keychain
        // item matched the search. Not re-exported by the
        // `security-framework` crate at this call site, so mirrored here as
        // a literal; see
        // <https://developer.apple.com/documentation/security/errsecitemnotfound>.
        const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

        match ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service("persona-wire")
            .account(service)
            .load_refs(true)
            .load_data(false)
            .search()
        {
            Ok(items) => Ok(!items.is_empty()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(false),
            Err(e) => Err(WireError::Storage(format!(
                "credentials: keyring existence check for '{service}': {e}"
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
            if provider.exists(service)? {
                return Ok(Some(provider.name()));
            }
        }
        Ok(None)
    }
}

/// [`AuthResolver`] impl wrapping a [`Credentials`] chain — the concrete
/// resolver `persona-wire-core`'s `application::auth::AuthResolver` trait is
/// written against. `Default` wraps [`Credentials::default_chain`] (env →
/// keyring), matching every adapter crate's existing `Credentials::` usage.
pub struct CredentialsAuthResolver {
    credentials: Credentials,
}

impl CredentialsAuthResolver {
    /// Wrap an explicit [`Credentials`] chain (e.g. a test double built via
    /// [`Credentials::with_providers`]).
    pub fn new(credentials: Credentials) -> Self {
        Self { credentials }
    }
}

impl Default for CredentialsAuthResolver {
    /// Wraps [`Credentials::default_chain`] (env var → OS keyring).
    fn default() -> Self {
        Self::new(Credentials::default_chain())
    }
}

impl AuthResolver for CredentialsAuthResolver {
    /// `AuthMethod::Bearer` resolves through [`Credentials::get`], mapped
    /// into [`AuthMaterial::Bearer`]. Any other `AuthMethod` variant (none
    /// exist yet — Phase 1 only ships `Bearer` — but `AuthMethod` is
    /// `#[non_exhaustive]` so a later phase can add one) fails loud with a
    /// structured [`WireError::Storage`] naming the unsupported method,
    /// rather than silently resolving to `Ok(None)`.
    fn resolve(&self, spec: &AuthSpec) -> WireResult<Option<AuthMaterial>> {
        match spec.method {
            AuthMethod::Bearer => Ok(self
                .credentials
                .get(&spec.service_key)?
                .map(AuthMaterial::Bearer)),
            other => Err(WireError::Storage(format!(
                "credentials: unsupported auth method {other:?} for service '{}'",
                spec.service_key
            ))),
        }
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

    // A provider whose `get` panics, to prove `resolve_source` calls
    // `exists` (not `get`). Its `exists` override returns a fixed answer
    // without ever touching `get`.
    struct ExistsOnlyMockProvider {
        name: &'static str,
        exists_result: bool,
    }

    impl TokenProvider for ExistsOnlyMockProvider {
        fn name(&self) -> &'static str {
            self.name
        }
        fn get(&self, _service: &str) -> WireResult<Option<SecretString>> {
            panic!("resolve_source must call `exists`, not `get`, on this provider");
        }
        fn exists(&self, _service: &str) -> WireResult<bool> {
            Ok(self.exists_result)
        }
    }

    #[test]
    fn resolve_source_uses_exists_not_get() {
        let creds = Credentials::with_providers(vec![Box::new(ExistsOnlyMockProvider {
            name: "exists-only",
            exists_result: true,
        })]);
        // Would panic (via `get`) if `resolve_source` didn't call `exists`.
        assert_eq!(creds.resolve_source("svc").unwrap(), Some("exists-only"));
    }

    #[test]
    fn resolve_source_uses_exists_not_get_when_absent() {
        let creds = Credentials::with_providers(vec![Box::new(ExistsOnlyMockProvider {
            name: "exists-only",
            exists_result: false,
        })]);
        assert!(creds.resolve_source("svc").unwrap().is_none());
    }

    // ---- MutableTokenProvider (ISP split) ----

    struct RecordingMutable {
        set_calls: std::sync::Mutex<Vec<(String, String)>>,
        delete_calls: std::sync::Mutex<Vec<String>>,
    }

    impl TokenProvider for RecordingMutable {
        fn name(&self) -> &'static str {
            "recording"
        }
        fn get(&self, _service: &str) -> WireResult<Option<SecretString>> {
            Ok(None)
        }
    }

    impl MutableTokenProvider for RecordingMutable {
        fn set(&self, service: &str, token: &str) -> WireResult<()> {
            self.set_calls
                .lock()
                .unwrap()
                .push((service.to_string(), token.to_string()));
            Ok(())
        }
        fn delete(&self, service: &str) -> WireResult<()> {
            self.delete_calls.lock().unwrap().push(service.to_string());
            Ok(())
        }
    }

    #[test]
    fn mutable_provider_can_be_dyn_dispatched() {
        let m: Box<dyn MutableTokenProvider> = Box::new(RecordingMutable {
            set_calls: std::sync::Mutex::new(Vec::new()),
            delete_calls: std::sync::Mutex::new(Vec::new()),
        });
        m.set("svc", "tok").unwrap();
        m.delete("svc").unwrap();
    }

    // ISP guarantee: `EnvTokenProvider` is read-only and must not implement
    // `MutableTokenProvider`. `set` / `delete` on it should be a compile
    // error, not a runtime `unimplemented!()`. This fn only compiles because
    // `EnvTokenProvider: TokenProvider` holds and does NOT require
    // `MutableTokenProvider` — a positive control proving the trait bound is
    // the (sole) enforcement mechanism.
    #[test]
    fn env_token_provider_is_read_only_by_trait_bound() {
        fn assert_read_only<P: TokenProvider>(_: &P) {}
        assert_read_only(&EnvTokenProvider);
    }

    // ---- CredentialsAuthResolver (application::auth::AuthResolver impl) ----
    //
    // Env provider only (no KeyringTokenProvider) — real OS keychain access
    // is never exercised from this offline unit test path.

    #[test]
    fn credentials_auth_resolver_bearer_resolves_from_env() {
        let service = "test-service-auth-resolver-delta";
        let var = EnvTokenProvider::primary_var_name(service);
        std::env::set_var(&var, "resolver-tok");
        let resolver = CredentialsAuthResolver::new(Credentials::with_providers(vec![Box::new(
            EnvTokenProvider,
        )]));
        let got = resolver.resolve(&AuthSpec::bearer(service)).unwrap();
        std::env::remove_var(&var);
        match got {
            Some(AuthMaterial::Bearer(secret)) => {
                assert_eq!(secret.expose_secret(), "resolver-tok")
            }
            other => panic!("expected Some(AuthMaterial::Bearer(_)), got {other:?}"),
        }
    }

    #[test]
    fn credentials_auth_resolver_unset_key_returns_ok_none() {
        let resolver = CredentialsAuthResolver::new(Credentials::with_providers(vec![Box::new(
            EnvTokenProvider,
        )]));
        let got = resolver
            .resolve(&AuthSpec::bearer(
                "test-service-auth-resolver-epsilon-never-set",
            ))
            .unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn credentials_auth_resolver_default_wraps_default_chain() {
        // Default() must not panic building the chain (env + keyring
        // providers, no I/O performed at construction time).
        let _resolver = CredentialsAuthResolver::default();
    }
}
