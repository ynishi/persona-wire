# persona-wire-core::application::auth

Indirect authentication reference layer for Adapter fetches
(`application::auth`).

## Why indirection

A bundle's `[[wirings]]` entry (and the adapter `source_uri` it carries)
is persisted as plain-text `Node.metadata` — it must never hold a secret
value. Instead, wiring / bundle authors write a **credential reference
key** (`service_key`), and secret resolution happens later, at fetch
time, through the existing [`persona-wire-credentials`] provider chain
(env var → OS keyring). This module defines the domain-facing vocabulary
for that reference:

```text
bundle TOML / source_uri            SQLite (plain text OK: key names only)
  wiring: source_uri = "github://owner/repo"
          auth = "github-alt"   ← reference key only, never a secret
                    │
                    ▼
  AuthSpec { service_key, method: AuthMethod }
                    │
                    │ AuthResolver::resolve
                    ▼
  AuthMaterial::Bearer(SecretString)
                    │
  exposed only at the transport boundary (e.g.
  `persona_wire_transport_http::HttpClient::with_bearer`)
```

[`AuthResolver`] is implemented by `persona-wire-credentials`'s
`CredentialsAuthResolver` (wraps `Credentials::default_chain()`), so this
crate stays free of any concrete credential-backend dependency — it only
owns the shape of the reference, not how it resolves.

## Phase 1 scope

Only [`AuthMethod::Bearer`] ships in Phase 1. `AuthMethod` and
[`AuthMaterial`] are both `#[non_exhaustive]` so that later phases
(`AtprotoSession` / `OAuth2` / token refresh) can add variants without a
breaking change; any `match` on either type from outside this crate must
carry a wildcard arm.

## Types

- `AuthMaterial` — Resolved authentication material — the actual secret, held as a
- `AuthMethod` — Authentication method carried by an [`AuthSpec`].
- `AuthSpec` — A wiring entry's authentication reference — **never** the secret itself.

## Traits

- `AuthResolver` — Resolves an [`AuthSpec`] into concrete [`AuthMaterial`], or `None` when

