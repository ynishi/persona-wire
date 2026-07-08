# persona-wire-core::infrastructure::wire_uri

WireUri — Wire 内で扱う URI の typed view (Layer 6 Adapter dispatch の共通入力)。

目的:
- URI grammar parse を **registry 一手に集約** (各 Adapter が strip_prefix を重複実装する
  drift を構造除去)。
- Adapter は `scheme()` / `host()` / `path()` / `query()` の typed access か、 互換のための
  `as_raw()` (full URI 文字列) のどちらかを選んで使う。

適用範囲 (RFC 3986 minimal subset):
- `scheme:[//authority]path[?query][#fragment]`
- authority は host 1 要素のみ (userinfo / port は parse しない、 必要になったら拡張)
- query は `key=value&key=value` flat form のみ (multi-value は最初の 1 個を採用)

ACL Facade 観点:
- URI 形式の定義責任は **Wire 側** (本 module + `PluginRegistry::route`)。
- 外部 SDK (persona-pack / mini-app / sqlite / file 等) の固有 grammar は Adapter 内に閉じる。

## Types

- `WireUri` — Parsed view of a `<scheme>://<host>/<path>?<query>#<fragment>` style URI。

