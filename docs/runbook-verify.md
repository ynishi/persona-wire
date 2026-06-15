# persona-wire — 動作検証 Runbook

手順 SoT (本 file = commit 対象、 version-track)。 verify 結果 trace は別 layer (internal workspace journal、 gitignored) で `TC-NNN: PASS/FAIL @ <commit> / <date>` form で 1 行 append 運用 (2 layer 分離: 本 file = 手順 / 別 layer = 実績)。

## 前提

- binary: `cargo install --path crates/persona-wire --force` で `~/.cargo/bin/persona-wire` 上書き済
- MCP: `.mcp.json` で `persona-wire` entry (`command = "persona-wire", args = ["mcp"]`) 配備済 + Claude Code restart 後 deferred tool `mcp__persona-wire__*` 利用可
- 検証 DB: `/tmp/wire-verify-<epoch>.db` (使い捨て、永続 `~/projects/persona-wire/persona-wire.db` には touch しない)
- workspace dir: `cd ~/projects/persona-wire`

## TC list

| TC | layer | 概要 | 自動化 | trigger |
|---|---|---|---|---|
| TC-001 | unit+integration | `cargo test --workspace` 55 PASS | ✓ cargo | 全 land |
| TC-002 | CLI E2E | init / node / edge / spec / projection / wire-init / wire-close 一気通貫 | 手動 | release-prep |
| TC-003 | MCP E2E | `wire_*` Tool 6 件経由で TC-002 同等 flow | 手動 | release-prep |
| TC-004 | Specification | And / Or / Not 合成 register + evaluate | 手動 | spec 変更時 |
| TC-005 | Projection | mustache nested field `{{a.b.c}}` expansion | 手動 | render 変更時 |
| TC-006 | graph 整合 | orphan node 検出 (wire_close report) | 手動 | release-prep |
| TC-007 | warning | dangling spec_ref warn (wire_init) | ✓ test | (test 済 carry) |
| TC-008 | エラー path | 不在 node に edge / duplicate node create | 手動 | エラー path 変更時 |
| TC-009 | wire_doctor | graph-wide 健全性 diagnostic (orphan + totals)、 wire_close と parity verify | 手動 + ✓ test | P2a land |
| TC-010 | bulk import | wire_nodes_create_batch / wire_edges_create_batch (1-row-at-a-time loop、 stops on first failure)、 happy path + duplicate stop + missing FK stop | ✓ test | P2c land |
| TC-011 | DB path resolution | env (`PERSONA_WIRE_DB`) > CLI `--db` flag > OS data dir (`$XDG_DATA_HOME/persona-wire/store.db` or `$HOME/.persona-wire/store.db`) の 3 段優先順 | ✓ test (env mutation) | path resolution 変更時 |

---

## TC-001: cargo test --workspace 52 PASS

- **目的**: 既存 unit + integration test の regression を機械検出
- **前提**: `cd ~/projects/persona-wire`、clean working tree (or 変更中なら commit 不問)
- **手順**:
  ```bash
  cargo test --workspace
  ```
- **期待**:
  - 終了 code = 0
  - `test result: ok. 49 passed; 0 failed` (persona-wire-core unit)
  - `test result: ok. 3 passed; 0 failed` (p1_e2e integration)
  - 合計 **52 PASS / 0 FAIL**
- **結果 trace**: journal Verified に `TC-001: PASS @ <commit> / <date>` 1 行

---

## TC-002: CLI 基本 E2E

- **目的**: CLI subcommand 7 件で graph 構築 → render → close 一気通貫を verify
- **前提**: 使い捨て DB path 確定 (`DB=/tmp/wire-verify-$(date +%s).db`)
- **手順**:
  ```bash
  DB=/tmp/wire-verify-$(date +%s).db
  persona-wire --db $DB init
  persona-wire --db $DB node create --id n1 --type persona --metadata '{"display":"n1"}'
  persona-wire --db $DB node create --id n2 --type persona --metadata '{"display":"n2"}'
  persona-wire --db $DB node create --id n3 --type persona --metadata '{"display":"n3"}'
  persona-wire --db $DB edge create --id e1 --src n1 --tgt n2 --kind routes_to
  persona-wire --db $DB edge create --id e2 --src n1 --tgt n3 --kind routes_to
  persona-wire --db $DB spec register --name active_personas --spec '{"TypeIs":"persona"}'
  persona-wire --db $DB projection register --name _persona_toc --spec-ref active_personas --template "Personas ({{count}}): {{names}}" --target-form prompt
  persona-wire --db $DB wire-init --persona-id n1
  persona-wire --db $DB wire-close --persona-id n1
  ```
- **期待**:
  - 各 subcommand 終了 code = 0
  - `wire-init` 出力に `Personas (3): n1, n2, n3` (順序は実装依存、3 件含む)
  - `wire-close` 出力に `total nodes: 3` / `total edges: 2` / `orphan: 0`
- **結果 trace**: journal Verified に `TC-002: PASS @ <commit> / <date> (DB=<path>)` 1 行
- **cleanup**: `rm $DB` (使い捨て DB は実走後削除)

---

## TC-003: MCP 基本 E2E (Tool 6 件)

- **目的**: JSON-RPC 経由で TC-002 同等 flow が動作することを verify (= CLI と MCP の parity)
- **前提**: Claude Code restart 済、`mcp__persona-wire__*` 6 Tool が deferred tool list に存在
- **手順** (各 Tool を順次):
  1. `mcp__persona-wire__wire_node_create(id="n1", type="persona", metadata={"display":"n1"})`
  2. `mcp__persona-wire__wire_node_create(id="n2", type="persona", metadata={"display":"n2"})`
  3. `mcp__persona-wire__wire_node_create(id="n3", type="persona", metadata={"display":"n3"})`
  4. `mcp__persona-wire__wire_edge_create(id="e1", src_node="n1", tgt_node="n2", kind="routes_to")`
  5. `mcp__persona-wire__wire_edge_create(id="e2", src_node="n1", tgt_node="n3", kind="routes_to")`
  6. `mcp__persona-wire__wire_spec_register(name="active_personas", spec={"TypeIs":"persona"})`
  7. `mcp__persona-wire__wire_projection_register(name="_persona_toc", spec_ref="active_personas", template="Personas ({{count}}): {{names}}", target_form="prompt")`
  8. `mcp__persona-wire__wire_init(persona_id="n1")`
  9. `mcp__persona-wire__wire_close(persona_id="n1")`
- **期待**:
  - 各 Tool call が success (error field 不在)
  - `wire_init` 返り値 projections に `_persona_toc` 含む + rendered に `Personas (3):` + n1/n2/n3 全 hit
  - `wire_close` 返り値 `total_node_count = 3` / `total_edge_count = 2` / `orphan_node_count = 0`
- **結果 trace**: journal Verified に `TC-003: PASS @ <commit> / <date>` 1 行
- **注**: 永続 DB (`.mcp.json` 配備の persona-wire.db) を使うので smoke 由来 row が累積する。clean state 検証は別 DB path で MCP entry 一時切替 or 別 verify session で carry

---

## TC-004: Specification 動的合成 (And / Or / Not)

- **目的**: Specification の合成 (And / Or / Not) が register → storage roundtrip → evaluate で動作する
- **前提**: TC-002 と同じ使い捨て DB
- **手順**:
  ```bash
  DB=/tmp/wire-verify-spec-$(date +%s).db
  persona-wire --db $DB init
  persona-wire --db $DB node create --id p1 --type persona --metadata '{"owner":{"name":"alpha"}}'
  persona-wire --db $DB node create --id p2 --type persona --metadata '{"owner":{"name":"beta"}}'
  persona-wire --db $DB node create --id n1 --type outline_node --metadata '{}'
  # And: TypeIs("persona") AND MetadataEq("owner.name", "alpha")
  persona-wire --db $DB spec register --name owned_by_alpha --spec '{"And":[{"TypeIs":"persona"},{"MetadataEq":{"path":"owner.name","value":"alpha"}}]}'
  persona-wire --db $DB projection register --name _owned --spec-ref owned_by_alpha --template "{{count}} matched: {{names}}" --target-form prompt
  persona-wire --db $DB wire-init --persona-id p1
  ```
- **期待**:
  - `wire-init` 出力に `1 matched: p1` (p2 は owner=beta で除外、n1 は persona でないので除外)
- **結果 trace**: journal Verified に `TC-004: PASS @ <commit> / <date>` 1 行
- **拡張 case** (carry):
  - Or: `{"Or":[{"TypeIs":"persona"},{"TypeIs":"outline_node"}]}` で 3 件 hit (p1/p2/n1)
  - Not: `{"Not":{"TypeIs":"persona"}}` で 1 件 hit (n1)

---

## TC-005: NamedProjection mustache rendering

- **目的**: mustache `{{a.b.c}}` の nested field expansion が動作する
- **前提**: TC-002 と同じ使い捨て DB
- **手順**: TC-004 の `template` に nested field を含む form で register
  ```bash
  persona-wire --db $DB projection register --name _nested --spec-ref active_personas --template "first: {{first.display}}" --target-form prompt
  ```
- **期待**: `wire-init` 出力に nested field が expand されている (現状 P1 = minimal mustache、`{{count}}` / `{{names}}` のみ実装、nested 未対応なら本 TC は `PARTIAL` で carry + Lua/Tera upgrade 軸 trigger)
- **結果 trace**: journal Verified に `TC-005: <PASS|PARTIAL|FAIL> @ <commit> / <date>` 1 行 + 不足 spec を carry note

---

## TC-006: orphan node 検出

- **目的**: edge に touch されない node が `wire_close` report で `orphan_node_count` に正しくカウントされる
- **前提**: 使い捨て DB
- **手順**:
  ```bash
  DB=/tmp/wire-verify-orphan-$(date +%s).db
  persona-wire --db $DB init
  persona-wire --db $DB node create --id n1 --type persona --metadata '{}'
  persona-wire --db $DB node create --id n2 --type persona --metadata '{}'
  persona-wire --db $DB node create --id orphan1 --type persona --metadata '{}'
  persona-wire --db $DB edge create --id e1 --src n1 --tgt n2 --kind routes_to
  persona-wire --db $DB wire-close --persona-id n1
  ```
- **期待**: `wire-close` 出力に `total nodes: 3` / `total edges: 1` / `orphan: 1`
- **結果 trace**: journal Verified に `TC-006: PASS @ <commit> / <date>` 1 行

---

## TC-007: dangling spec_ref warning (test 済 carry)

- **目的**: `wire_init_warns_on_dangling_spec_ref` integration test が PASS
- **前提**: TC-001 で全 test PASS なら本 TC も内包される
- **手順**:
  ```bash
  cargo test --workspace --test p1_e2e wire_init_warns_on_dangling_spec_ref
  ```
- **期待**: 終了 code = 0 / `test result: ok. 1 passed`
- **結果 trace**: TC-001 と同章で記録 (`TC-007: PASS (included in TC-001)`)

---

## TC-008: エラー path

- **目的**: 不在 node への edge / duplicate node create が graceful error で fail (panic しない)
- **前提**: 使い捨て DB
- **手順** (期待 = いずれも非 0 exit + error message 出力):
  ```bash
  DB=/tmp/wire-verify-err-$(date +%s).db
  persona-wire --db $DB init
  persona-wire --db $DB node create --id n1 --type persona --metadata '{}'
  # E1: 不在 node に edge
  persona-wire --db $DB edge create --id e1 --src n1 --tgt nonexistent --kind routes_to ; echo "exit=$?"
  # E2: duplicate node
  persona-wire --db $DB node create --id n1 --type persona --metadata '{}' ; echo "exit=$?"
  # E3: 不在 spec で projection register
  persona-wire --db $DB projection register --name p --spec-ref missing --template "x" --target-form prompt ; echo "exit=$?"
  ```
- **期待**:
  - E1: exit != 0、error に `tgt_node` / `nonexistent` / `not found` 等の hint
  - E2: exit != 0、error に `duplicate` / `UNIQUE constraint` 等の hint
  - E3: 実装依存 (現状 register 時の spec_ref 存在検証は wire_init 側の warn 経路、register 自体は PASS。期待 = exit = 0 だが wire_init で warn surface)
- **結果 trace**: journal Verified に `TC-008: <PASS|FAIL per case> @ <commit> / <date>` 1 行 + 各 case 結果

---

## TC-009: wire_doctor (graph-wide 健全性 diagnostic)

- **目的**: `wire_doctor` Tool が graph 全体の orphan + totals を `wire_close` と parity で報告することを verify
- **前提**: 使い捨て DB (`/tmp/wire-verify-doctor-<epoch>.db`)
- **手順**:
  ```bash
  DB=/tmp/wire-verify-doctor-$(date +%s).db
  persona-wire --db $DB init
  persona-wire --db $DB node create --id n1 --type persona --metadata '{}'
  persona-wire --db $DB node create --id n2 --type persona --metadata '{}'
  persona-wire --db $DB node create --id orphan1 --type persona --metadata '{}'
  persona-wire --db $DB edge create --id e1 --src n1 --tgt n2 --kind routes_to
  persona-wire --db $DB wire-doctor
  persona-wire --db $DB wire-close --persona-id n1
  ```
- **期待**:
  - `wire-doctor` 出力に `# wire_doctor report` header + `total nodes: 3` / `total edges: 1` / `orphan nodes (0 in + 0 out): 1`
  - `wire-close` と数値が完全一致 (parity check)
- **結果 trace**: 別 layer Verified に `TC-009: PASS @ <commit> / <date>` 1 行
- **自動化 carry**: `tests/p2a_e2e.rs` の `wire_doctor_parity_with_wire_close_on_same_graph` / `wire_doctor_reports_orphan_zero_when_every_node_is_touched` / `wire_doctor_with_dynamic_specification_e2e` 3 件で内蔵 verify (TC-001 内包)

### MCP form

CLI と同等 flow を MCP Tool 経由で:

1. `mcp__persona-wire__wire_node_create` × 3
2. `mcp__persona-wire__wire_edge_create` × 1
3. `mcp__persona-wire__wire_doctor()` (引数なし、 graph-wide)
4. `mcp__persona-wire__wire_close(persona_id=...)` (parity check 用)

- 期待: `wire_doctor` 返り値の markdown が `# wire_doctor report` で始まり、 wire_close と数値一致

---

## TC-010: bulk import (wire_nodes_create_batch / wire_edges_create_batch)

- **目的**: 大量 row の graph 投入を 1 Tool call にまとめて Tool-call 連打負担を排除、 fail 時の半端 insert state を visible 化 (= inserted_count + failed_at)
- **前提**: in-memory or 使い捨て DB、 type vocabulary seed 済
- **手順 (test 内蔵 3 case)**:
  1. **happy path** (`tests/p2c_e2e.rs::batch_inserts_all_nodes_and_edges_happy_path`): 3 node + 2 edge を batch insert、 inserted_count=3 / failed_at=None、 wire_doctor で graph 整合確認
  2. **duplicate stop** (`tests/p2c_e2e.rs::batch_stops_at_first_duplicate_node`): 既存 id と衝突する row を batch 2 件目に置く、 inserted_count=1 / failed_at=Some(1) + error_message に `UNIQUE` / `constraint` hint
  3. **missing FK stop** (`tests/p2c_e2e.rs::batch_stops_at_first_edge_missing_node`): 不在 node を tgt に持つ edge を batch 中盤に置く、 inserted_count + failed_at で部分 insert state surface
- **期待**: 全 case PASS、 fail 時の partial state が caller (外側 LLM) に見える form で返る
- **結果 trace**: TC-001 内包 (`TC-010: PASS (included in TC-001)`)

### Semantic 注意

- **非 atomic**: 1-row-at-a-time loop で fail 時に停止、 既挿入は rollback されない。 外側 LLM が `inserted_count` + `failed_at` を見て retry / patch 判断する form
- **既存単発 Tool** (`wire_node_create` / `wire_edge_create`) は維持。 1 件投入の小回りは単発、 N 件投入は batch、 と使い分け
- **atomic Tx wrap は P2c では carry**: usage 観察後に「partial insert で graph 壊れるケースが頻発するか」 を判断 trigger に。 「動かしながら書く」 規律

---

## TC-011: DB path resolution (env > flag > fallback)

- **目的**: persona-x family 規約 (persona-work pattern) に揃えた path resolution が env / flag / OS data dir の 3 段優先順で動作することを verify
- **前提**: `crates/persona-wire-core/src/infrastructure/storage.rs:default_db_path()` + `crates/persona-wire/src/main.rs` 内 path resolution code
- **手順 (test 内蔵 3 case、 `tests/db_path_resolution.rs`)**:
  1. `fallback_uses_xdg_data_home_when_set`: `XDG_DATA_HOME=/tmp/test-xdg-data` set → `/tmp/test-xdg-data/persona-wire/store.db`
  2. `fallback_uses_home_dotfile_when_xdg_unset`: `XDG_DATA_HOME` unset + `HOME=/tmp/test-home` → `/tmp/test-home/.persona-wire/store.db`
  3. `fallback_errors_when_neither_xdg_nor_home_is_set`: 両方 unset → `WireError::Storage("HOME not set")` 期待
- **期待**: 3 case 全 PASS、 env mutation は `Mutex` + RAII `EnvSnapshot` で serialise + 復元
- **結果 trace**: TC-001 内包 (`TC-011: PASS (included in TC-001)`)

### CLI 経路 (手動 smoke)

```bash
# env 優先
PERSONA_WIRE_DB=/tmp/env-test.db persona-wire wire-doctor

# flag 優先 (env unset 時)
unset PERSONA_WIRE_DB
persona-wire --db /tmp/flag-test.db wire-doctor

# fallback (env / flag 両方 unset)
persona-wire wire-doctor
# → ~/.persona-wire/store.db (or $XDG_DATA_HOME/persona-wire/store.db) 着地確認
```

### 規律 (持続的な汚染防止)

- **project root 直下に DB が着地しない**こと: 着地点が `~/projects/persona-wire/persona-wire.db` のような CWD 相対になっていないことを毎回 verify
- `.mcp.json` の `env.PERSONA_WIRE_DB` block は削除済 (default で OS data dir 着地)、 必要なら User 手動 env override
- runbook 各 TC の `--db /tmp/wire-verify-*.db` pattern は引き続き CLI flag 経路で OS dir 汚染を回避

---

## CLI ↔ MCP IF parity (semantic-first canonical)

CLI と MCP Tool param は semantic-first で literal 揃え (kebab ↔ snake 変換のみ)。旧 form は alias で残置 (backward compat)。

| 操作 | CLI canonical | CLI alias (旧) | MCP param |
|---|---|---|---|
| spec body 渡し | `--spec` | `--json` | `json` (Tool は `json` 維持、 backward compat) |
| persona 指定 (wire-init / wire-close) | `--persona-id` | `--persona` | `persona_id` |

`--json` / `--persona` は backward compat alias、 既存 script / 旧 runbook は alias 経由で動作継続。 canonical form は新規記述で使う。

## 履歴

- 2026-06-15: TC-001〜TC-008 起こし (commit `7e1000a` P2-RELEASE-PREP land 直後の initial draft)
- 2026-06-15: CLI ↔ MCP IF parity 反映 — CLI `--json` → `--spec` canonical + `json` alias、 `--persona` → `--persona-id` canonical + `persona` alias、 cargo test 52 PASS 維持 + smoke (canonical + alias 両方動作確認)
- 2026-06-15: P2a `wire_doctor` land — graph-wide 健全性 diagnostic Tool 追加 (Orphan 1 軸、 `wire_close` の orphan logic を `graph_scan_summary` pub fn として切り出し共有)、 CLI subcommand + MCP Tool + integration test 3 件追加 (`tests/p2a_e2e.rs` 新規)、 cargo test 55 PASS (49 unit + 3 p1_e2e + 3 p2a_e2e)、 TC-009 起こし
- 2026-06-15: P2c bulk import Tool land — `wire_nodes_create_batch` / `wire_edges_create_batch` MCP Tool 追加 (1-row-at-a-time loop、 stops on first failure、 inserted_count + failed_at + error_message 返り値)、 CLI は carry (別 turn)、 integration test 3 件追加 (`tests/p2c_e2e.rs` 新規)、 cargo test 58 PASS (49 + 3 + 3 + 3)、 TC-010 起こし。 atomic Tx wrap は usage 観察後 carry
- 2026-06-15: DB path resolution fix — `DEFAULT_DB = "./persona-wire.db"` (CWD 相対 hardcoded) を persona-x family 規約 (persona-work pattern) に揃える、 `storage::default_db_path()` helper 新規 (XDG_DATA_HOME > $HOME/.persona-wire/store.db fallback)、 `main.rs` で env (`PERSONA_WIRE_DB`) > CLI `--db` > helper の 3 段優先順実装、 `.mcp.json` の env block 削除、 integration test 3 件 (`tests/db_path_resolution.rs` 新規)、 cargo test 61 PASS (49 + 3 + 3 + 3 + 3)、 TC-011 起こし。 project root 直下汚染 bug 解消

## 運用 SOP

1. **新規 verify scope 発生時**: 連番で TC 追加 + docs commit
2. **verify 実走時**: TC を順次走らせ、journal Verified に `TC-NNN: PASS/FAIL @ <commit> / <date>` 1 行記録
3. **既存 TC 変更時**: 本 file 修正 commit + 旧 result は journal で historical reference
4. **TC retire**: 番号は空き番のまま (= 飛び番容認、renumber 禁止 = audit trail 保全)
