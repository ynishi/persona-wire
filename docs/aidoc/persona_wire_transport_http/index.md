# persona-wire-transport-http 0.12.1

Shared HTTP transport for persona-wire Adapters (`persona-wire-transport-http`).

## Architecture

[`HttpClient`] is a builder-style, stateless-per-call HTTP client shared by
every HTTP-backed Adapter crate. It carries no scheme-specific knowledge
(no feed parsing, no API-specific response shaping) — that stays in the
calling Adapter. `ctx` is a short, human-readable prefix (e.g.
`"rss adapter"`) baked into every error message so failures are traceable
back to the Adapter that produced them, matching the message form each
Adapter used before this crate existed.

```text
RssAdapter, NotionAdapter, ... (scheme-specific parse / normalize)
       │
       ▼
HttpClient::new(ctx).with_timeout(..).with_bearer(..).with_header(..)
       │
       ▼
reqwest::Client (rustls-tls, per-call)
```

## API

- [`HttpClient::new`] takes only `ctx`; [`DEFAULT_TIMEOUT`] applies unless
  overridden via [`HttpClient::with_timeout`].
- [`HttpClient::with_bearer`] sets an `Authorization: Bearer <token>`
  header from a [`secrecy::SecretString`].
- [`HttpClient::with_header`] appends an arbitrary fixed header (e.g.
  `Notion-Version`).
- [`HttpClient::get_bytes`] / [`HttpClient::get_json`] /
  [`HttpClient::post_json`] perform the request; JSON variants parse the
  response body as [`serde_json::Value`].

## Error conventions

Every failure is [`persona_wire_core::WireError::Storage`] with a
`"{ctx}: <what> fetching '{url}': {cause}"` shaped message (see the
`*_err` helpers in this module for the exact wording per failure kind).
Non-2xx HTTP status is treated as a fetch failure, not a partial success.

