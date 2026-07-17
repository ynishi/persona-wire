# persona-wire-adapter-sqlite-x 0.14.0

persona-wire Adapter for raw SQLite SoT (scheme `sqlite://`).

Single-binary OSS distribution / Fly.io self-hosting (P4 roadmap) で
鉄板になる adapter — mini-app schema convention に縛られず、 任意の
SQLite file に対して直接 SQL を投げる generic backend。 volume mount
1 個でそのまま動く。

## URI form

```text
sqlite://<path>?<query|table>=<value>[&limit=<n>]
```

- `<path>` — file path。 `~/` で始まる場合は HOME 展開。 host+path 双方を URL から
  組み直すため `sqlite:///abs/path.db` でも `sqlite://./relative.db` でも `sqlite://~/.db`
  でも受け取れる
- `?query=<URL-encoded SQL>` — primary form。 任意の SELECT (or PRAGMA) を実行
- `?table=<name>` — sugar (= `SELECT * FROM "<name>"` に展開)。 `query` と排他
- `?limit=<n>` — sugar form の `LIMIT` 句として付与 (primary form では行数 cap として
  適用、 SQL 本体には触らない)

## Return shape

```jsonc
{
  "scheme": "sqlite",
  "path": "/abs/path/to.db",
  "count": 3,
  "rows": [
    {"id": 1, "name": "alice"},
    {"id": 2, "name": "bob"},
    {"id": 3, "name": null}
  ]
}
```

BLOB column は base64 encoded string になる (`data:base64,<...>` prefix なし、
純粋な base64 文字列)。 wire の prompt context に直接埋めるのは想定していない、
caller (template / projection) 側で必要なら decode する。

