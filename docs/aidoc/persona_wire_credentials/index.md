# persona-wire-credentials 0.12.0

Provider-chain credential resolution for persona-wire Adapters
(`persona-wire-credentials`).

## Architecture

```text
Credentials::default_chain()
       │
       ├─ EnvTokenProvider     (checked first)
       └─ KeyringTokenProvider (checked second, OS keychain)
               │
               ▼
       Option<secrecy::SecretString>
```

[`Credentials`] holds an ordered list of [`TokenProvider`] impls and
queries them in order, returning the first `Some(token)`. Any adapter
needing an external-service API token asks `Credentials::get(service)`
rather than reading `std::env` or the OS keychain directly, so the
precedence and fail-loud behavior below is uniform across every adapter.

## Precedence

For a given `service` (e.g. `"github"`), [`EnvTokenProvider`] checks, in
order:

1. `PERSONA_WIRE_TOKEN_<SERVICE>` (`service` upper-cased, `-` → `_`).
2. The conventional alias env var in [`ALIAS_ENV_VARS`], if `service` has
   one (e.g. `github` → `GITHUB_TOKEN`).

An env var set to the empty string is treated as absent (`None`), not as
an empty token. [`KeyringTokenProvider`] is checked only if no env
provider (in [`Credentials::default_chain`], `EnvTokenProvider`) supplied
a token.

## Security notes

- Every token is [`secrecy::SecretString`] end to end; call
  `secrecy::ExposeSecret::expose_secret` only at the point of use (e.g.
  building an `Authorization` header), never for logging or `Debug`.
- Never place a token in a `source_uri` or any other logged/printed
  value.
- **Fail loud**: a provider-level error (e.g. keychain access denied)
  propagates as `Err`, it is never silently swallowed into `Ok(None)`.
  Only "this provider has no entry for this service" is `Ok(None)`.

