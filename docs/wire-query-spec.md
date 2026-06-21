# wire_query — Ad-hoc Specification Query (Spec)

graph 全体に対する **ad-hoc query Tool**。 既存 `Specification` AST を JSON DSL として直接受け取り、 matched Node の slim list を返す。 mini-app の `list(table, filter)` と semantic 対応 (table = 全 Node、 filter = Specification)。

P2b land (commit pending、 `feat(p2b): land wire_query`)。

---

## 1. Tool surface

### 1.1 MCP

```
mcp__persona-wire__wire_query({
  spec?:      string  // Specification AST literal (JSON)
  spec_ref?:  string  // 登録済 Specification の name
  limit?:     usize   // 戻り件数上限、 omit で unlimited
  offset?:    usize   // 先頭スキップ数、 omit で 0
})
```

- `spec` と `spec_ref` は **排他** (どちらか 1 つ必須)、 両方 set / 両方 omit はいずれも `InvalidSpec` error
- 不在 `spec_ref` は `NotFound("spec: <name>")`

### 1.2 CLI

```bash
persona-wire query [--spec <JSON> | --spec-ref <NAME>] [--limit <N>] [--offset <N>]
```

- `--spec` と `--spec-ref` は clap `conflicts_with` で **同時指定 reject**
- 出力は **JSON pretty** (既存 CLI 出力 form と整合)

---

## 2. Specification AST (Query DSL)

`Specification` は JSON で直接 AST 表現できる DSL。 既存 `wire_spec_register` で register する spec body と完全に同じ form (= register / ad-hoc を同 DSL で書ける一貫性)。

### 2.1 Leaf forms

| form | semantic |
|---|---|
| `{"TypeIs": "<type_name>"}` | Node の `type` field が `<type_name>` と完全一致 |
| `{"MetadataEq": {"path": "<dotted.path>", "value": <json_value>}}` | Node の `metadata.<dotted.path>` 値が `<json_value>` と完全一致 |

`MetadataEq.path` は dotted form の JSON path (例: `"status"` / `"owner.name"` / `"_meta.source_table"`)、 階層 metadata に対応。

### 2.2 Compositional forms

| form | semantic |
|---|---|
| `{"And": [<spec>, <spec>, ...]}` | 全 sub-spec を満たす (= AND) |
| `{"Or": [<spec>, <spec>, ...]}` | いずれかの sub-spec を満たす (= OR) |
| `{"Not": <spec>}` | sub-spec を満たさない (= NOT) |

ネスト可能 (`And` の中に `Or`、 `Or` の中に `Not` 等)。

### 2.3 BNF 風

```
Spec ::= Leaf | Composite

Leaf ::= TypeIs | MetadataEq
TypeIs     ::= { "TypeIs": String }
MetadataEq ::= { "MetadataEq": { "path": String, "value": JsonValue } }

Composite ::= And | Or | Not
And ::= { "And": [Spec, Spec, ...] }
Or  ::= { "Or":  [Spec, Spec, ...] }
Not ::= { "Not": Spec }
```

### 2.4 例

```json
// persona 全件
{"TypeIs":"persona"}

// owner=alpha の persona
{"And":[
  {"TypeIs":"persona"},
  {"MetadataEq":{"path":"owner.name","value":"alpha"}}
]}

// outline_node でない node (= persona / ma_row / etc)
{"Not":{"TypeIs":"outline_node"}}

// open issue (mini-app row) OR pending issue
{"And":[
  {"TypeIs":"ma_row"},
  {"Or":[
    {"MetadataEq":{"path":"status","value":"open"}},
    {"MetadataEq":{"path":"status","value":"pending"}}
  ]}
]}
```

---

## 3. 戻り値 form

```json
{
  "matched": [
    {
      "id":       "<node_id>",
      "type":     "<node_type>",
      "metadata": <json_value>
    },
    ...
  ],
  "total_count":    <usize>,   // limit/offset 前の全 hit 数
  "returned_count": <usize>    // 実際に matched に入った件数 (= matched.len())
}
```

### 3.1 Slim Node form

戻り値の Node は `{id, type, metadata}` のみ (= **slim form**)。 以下 field は **含まれない**:

- `sot_ref`
- `confidence`
- `applicability`
- `last_verified_at`
- `review_due`
- `version`
- `prev_id`

理由 = token 量と情報密度のトレードオフ。 full Node を欲しい場合は別 Tool (将来 carry、 `wire_get` 等の単発 lookup) を検討。

### 3.2 field-level output filter (carry)

mini-app の `output_fields` 相当 (= 戻り値 field を caller が絞る) は別 surface 候補 (例: `wire_select` 等)、 P2b では入れず carry。 trigger = 「slim form でも metadata が大きすぎて token 効率悪い」 が surface した時。

---

## 4. limit / offset semantic

### 4.1 limit precedence (CLI 経路)

```
CLI --limit flag > env PERSONA_WIRE_QUERY_LIMIT > None (unlimited)
```

- CLI `--limit 10`: そのまま採用
- CLI 省略 + `PERSONA_WIRE_QUERY_LIMIT=100`: env 値採用
- 両方 omit: `None` = 全件返却

MCP 経路では env fallback なし (caller の責務、 caller layer で env 解釈してから `limit` field に流す form)。

### 4.2 offset

- `Some(N)`: 先頭 N 件スキップ
- `None`: 0 と等価
- offset 超過 (collection size より大): `returned_count = 0`、 `total_count` は変わらず

### 4.3 ordering

現状 ordering は **storage の natural order** (= 内部 `list_types_by_kind` + `list_nodes_by_type` の SQLite order)。 caller 側で必要なら sort を別途実施。 explicit `sort_by` 引数は P2b では入れず carry (= aggregate / sort は別 surface)。

---

## 5. spec / spec_ref 排他規律

| spec | spec_ref | 動作 |
|---|---|---|
| Some | None | inline spec を評価 |
| None | Some | registered spec を name lookup → 評価 (不在なら `NotFound`) |
| Some | Some | `InvalidSpec("spec and spec_ref are mutually exclusive")` |
| None | None | `InvalidSpec("either spec or spec_ref is required")` |

CLI 経路は clap `conflicts_with` で **CLI parse 段階で reject**、 use case に到達しない。 MCP 経路は use case 内 validation で reject。

---

## 6. 既存 surface との関係

| Tool | scope | wire_query との関係 |
|---|---|---|
| `wire_init` | 全 NamedProjection を一括 render | 個別 spec lookup ではなく projection bundle、 wire_query は raw node、 wire_init は rendered string |
| `wire_close` | persona scope lifecycle scan + orphan report | persona 中心、 wire_query は graph 全体 |
| `wire_doctor` | graph-wide health diagnostic (orphan + totals) | 集計値、 wire_query は detail node list |
| `wire_node_create` / `wire_edge_create` | 単発 CRUD | input 系、 wire_query は output 系 |
| `wire_nodes_create_batch` / `wire_edges_create_batch` | bulk input | input 系 (P2c) |
| `wire_spec_register` | Specification を name で永続化 | wire_query の `spec_ref` 経路で参照される source |
| `wire_projection_register` | NamedProjection (spec + template + target_form) を name で永続化 | wire_init / 将来 wire_render の source |

### 6.1 mini-app との対応

| mini-app | wire (persona-wire) |
|---|---|
| `mcp__mini-app__list(table, filter, limit, offset)` | `wire_query(spec, limit, offset)` |
| filter `{type: "eq", field: "...", value: ...}` | `MetadataEq` |
| filter `{type: "and", filters: [...]}` | `And` |
| filter `{type: "or", filters: [...]}` | `Or` |
| filter `{type: "in", field: "...", values: [...]}` | (carry: `MetadataIn` 等を追加候補) |
| `output_fields` (field 絞り) | (carry: `wire_select` 等別 surface) |
| `data_snapshot` (table dump) | (scope 外、 graph dump は別 Tool 検討) |

ある程度 1:1 写像。 mini-app の query 知識をそのまま流用可能。

---

## 7. Use case 例

### 7.1 persona Node 全件取得

```bash
persona-wire query --spec '{"TypeIs":"persona"}'
```

外側 LLM が active persona set を構築する起点 (例: `/wake` 拡張で 「現在 graph 上の persona list」 を inject)。

### 7.2 動的 metadata filter (ad-hoc)

```bash
persona-wire query --spec '{"And":[
  {"TypeIs":"ma_row"},
  {"MetadataEq":{"path":"status","value":"open"}}
]}'
```

= 「open issue 全件」 のような ad-hoc query。 register せず 1 回限りの探索。

### 7.3 spec_ref 経路 (register 済 spec の再利用)

```bash
# 事前 register
persona-wire spec register --name active_personas --spec '{"And":[
  {"TypeIs":"persona"},
  {"MetadataEq":{"path":"status","value":"active"}}
]}'

# query 時は name 参照
persona-wire query --spec-ref active_personas
```

頻用 query を register、 別 session / 別 Tool (`wire_init` の projection 等) と共有。

### 7.4 pagination (大量 hit)

```bash
# 第 1 ページ
persona-wire query --spec '{"TypeIs":"ma_row"}' --limit 50

# 第 2 ページ
persona-wire query --spec '{"TypeIs":"ma_row"}' --limit 50 --offset 50
```

### 7.5 MCP 経路 (外側 LLM が自動構築)

```js
// row source → wire Node 取り込み後、 動的 query
mcp__persona-wire__wire_query({
  spec: JSON.stringify({
    And: [
      { TypeIs: "ma_row" },
      { MetadataEq: { path: "_meta.source_table", value: "<table>" } },
      { MetadataEq: { path: "to", value: "<recipient>" } }
    ]
  }),
  limit: 20
})
// → "<recipient> 宛 row 直近 20 件"
```

---

## 8. Future expansion (carry)

| 軸 | 概要 | trigger |
|---|---|---|
| ~~`wire_render`~~ **land 済** | register 済 NamedProjection を name 指定で個別 render (wire_init の counterpart)、 戻り値 `{name, target_form, rendered}`、 dangling spec_ref / 不在 projection_ref は `NotFound` error。 ad-hoc inline (spec + template + target_form 直渡し) は carry | wire_init で「全 projection 一括」 が token 重い時の選択肢として land |
| field-level output filter | 戻り値 Node の field 絞り (例: `--fields id,metadata.status`)、 mini-app `output_fields` 相当 | slim form でも metadata が大きすぎる時 |
| `MetadataIn` / `MetadataGt` / `MetadataLt` 等の Leaf 拡張 | array 包含 / 数値比較 / 範囲 | expressiveness 不足 surface 時 |
| sort / aggregate (count / sum / group_by) | ordering + 集計 | 大量 hit で client 側 sort/agg が token 重くなる時 |
| Mlua 統合 (Specification を Lua snippet で表現) | custom predicate / 任意計算 | JSON DSL の表現力不足 surface 時、 concept-doc §6 Template DSL Lua 軸と sibling 判断 |
| atomic Tx wrap | bulk import / query を Tx で wrap | partial state による graph 壊れが頻発した時 (P2c carry と sibling) |

各軸とも **usage 観察で trigger** が出てから着手 = 「動かしながら書く」 規律準拠、 vision-driven の前倒し実装は避ける。

---

## 9. Implementation note

| layer | file | 概要 |
|---|---|---|
| Domain Core | `crates/persona-wire-core/src/domain/specification.rs` | `Specification` enum + `is_satisfied_by(node)` メソッド (P1 land) |
| Application | `crates/persona-wire-core/src/application/use_cases.rs` | `wire_query` use case + `WireQueryInput` / `WireQueryNode` / `WireQueryOutput` |
| Application | `crates/persona-wire-core/src/application/spec_registry.rs` | `SpecRegistry::get(name)` (spec_ref 経路で参照) |
| Surface (MCP) | `crates/persona-wire-mcp/src/lib.rs` | `wire_query` MCP Tool + `WireQueryParams` |
| Surface (CLI) | `crates/persona-wire/src/main.rs` | `Command::Query(QueryArgs)` + env `PERSONA_WIRE_QUERY_LIMIT` fallback |
| Test | `crates/persona-wire-core/tests/p2b_e2e.rs` | 4 case (inline / spec_ref / pagination / validation) |
| Runbook | `docs/runbook-verify.md` | TC-012 |

### 9.1 collect_matching_nodes (内部 helper)

`use_cases.rs:collect_matching_nodes` は wire_init / wire_query で共有。 全 node type を iterate して `Specification::is_satisfied_by` で filter する shared scan primitive。 P2a の `graph_scan_summary` 同型 pattern。

### 9.2 env fallback の責務分離

- **CLI 経路**: `main.rs` で env `PERSONA_WIRE_QUERY_LIMIT` 解釈 → `WireQueryInput.limit` に詰めて use case に渡す
- **MCP 経路**: caller (外側 LLM / MCP client) の責務、 use case 自体は `limit: Option<usize>` を取るだけ
- = env 解釈は surface layer の責務、 use case は pure に保つ

---

## 関連

- `docs/_archive/concept-2026-06-14.md` §3 Architecture / §3.1 Specification (BP 由来) / §7 Phase plan (P0 設計 trace、 land 済)
- `crates/persona-wire-core/src/lib.rs` `//!` block — 設計 SoT (Layer split / Render flow / Persistence schema)
- `docs/runbook-verify.md` TC-012 (wire_query 動作検証手順)
- `crates/persona-wire-core/tests/p2b_e2e.rs` (integration test 4 case)
