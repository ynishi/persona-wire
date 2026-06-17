# wire_workflow_* — Declarative Loop / Trigger / Action Spec (P5 seed)

`onboarding.md §6b` で示した *Loop / review / update-check* UC を、 caller
側の Skill / Hook / cron 配線に依存せず **wire 内 data として宣言** する
ための Tool surface 設計 draft。 `concept-2026-06-14.md` Phase plan の
**P5** に対応。 これは叩き台です。 修正があれば指摘してください。 全然違って
いれば破棄します。

---

## 0. Scope split

| Phase | Scope | 本 doc 範囲 |
|---|---|---|
| **P5 (本 doc)** | declarative `wire_workflow_*` Tool surface — workflow を data として登録 / 列挙 / fire / 削除。 caller (Skill / Hook / cron) が fire を引く。 | **対象** |
| **P3a / P3c** (carry) | Daemon (cron tick + Lifecycle scan + Output channel emit)。 daemon が workflow を自走 fire する。 | 対象外 (sibling) |
| **wire_update** (P5 sibling) | cross-ref 自動維持 (Node A 変更 → 依存 Node B の review_pending 自動立て)。 | §6 で輪郭のみ |

MVP は「workflow を data 化、 caller が fire」 で閉じる。 daemon は P5 land
後の usage pattern 観察を経て P3a/P3c で着手する。

---

## 1. Mental model

Workflow = **(Trigger, Action) の組を data として表現した 1 個の Node** + その
fire を起動する Tool。

```
Trigger  : 「いつ / 何があれば」 fire するか (cron / on_event / on_demand)
Action   : 「何を行うか」 (emit projection / set metadata / fire mailbox / no-op)
```

Specification / NamedProjection が「graph 観察の declarative DSL」 だった
のと対称に、 Workflow は **「graph 上での action の declarative DSL」**。

既存 primitive との合流:

- Trigger 条件で graph を query する → `Specification`
- Action で render する → `NamedProjection`
- Action 結果を外部 SoT へ書く → `Adapter` (将来 P3b で write-path 拡張時)

= 新規 mechanism を追加せず、 既存 primitive を **時間軸 (trigger) + 副作用
軸 (action)** で組み合わせる薄い layer。

---

## 2. Data model — Workflow Node

既存 Node type `workflow_def` (type_registry seed 済、 `concept-2026-06-14.md`
§4.2 9 種の 1 つ) を流用。 schema 追加なし。 Spec / Projection と同様 1
Tool で register、 graph 上の他 Node と同じく `wire_query` で観察可能。

```jsonc
{
  "id":   "alpha.workflow.review_close",
  "type": "workflow_def",
  "metadata": {
    "persona": "alpha",
    "trigger": {
      "kind":  "on_event",
      "event": "session_close"
    },
    "action": {
      "kind":             "emit_projection",
      "projection_names": ["review_pending"]
    },
    "enabled":         true,
    "last_fired_at":   null,
    "last_fire_status": null
  }
}
```

`metadata.trigger.kind` と `metadata.action.kind` が discriminator。 残りは
kind 固有の field。

---

## 3. Trigger forms

| kind | required fields | semantic |
|---|---|---|
| `on_demand` | (なし) | caller が `wire_workflow_fire` を明示 invoke した時だけ fire。 default |
| `on_event` | `event: string` | caller が `wire_workflow_fire` 時に `event` 名を渡す。 一致した workflow のみ fire (caller-driven event bus) |
| `cron` | `cron_spec: string` (5-field crontab)、 `timezone: string` (default `UTC`) | P3a daemon land 時のみ自走、 daemon 前は `on_demand` と同等 |
| `metadata_changed` | `watch_spec: Specification` | watched Node の metadata が変化した時 fire (`wire_update` 経路、 §6 参照) |

`cron` と `metadata_changed` は **daemon 前提**。 daemon 未着地段階では
caller が tick を引かない限り fire しない (= 退化して `on_demand` 動作)。
fail-loud にせず silent fall-through するのは「P5 単独で land 可能」 を保つ
ため。

---

## 4. Action forms

| kind | required fields | semantic |
|---|---|---|
| `emit_projection` | `projection_names: string[]` | `wire_prompt_context(persona_id, projection_names)` 相当を内部 invoke して結果を fire result に同梱 |
| `set_metadata` | `target_spec: Specification`、 `patch: object` | matched Node の metadata に shallow merge (= `wire_update` への足場、 §6) |
| `fire_mailbox` | `to: string`、 `subject: string`、 `body_template: string` | mini-app `mailbox` row 1 件 create (Layer 6 Adapter `mini-app://` write-path、 P3b 領分) |
| `no_op` | (なし) | trace 用 (workflow が登録されているだけで fire log を残す) |

action は **1 workflow = 1 action**。 chain したい場合は workflow を
複数 register (= 後で graph 化して visualizable に保つ)。

---

## 5. Tool surface

```
mcp__persona-wire__wire_workflow_register({
  id:       string,    // 必須、 Node id (例: "alpha.workflow.review_close")
  persona_id?: string, // metadata.persona 用 scope
  trigger:  string,    // §3 form を **JSON string** で渡す (wire_spec_register.json と同型)
  action:   string,    // §4 form を **JSON string** で渡す
  enabled?: boolean    // default true
}) -> "registered workflow: <id>"

mcp__persona-wire__wire_workflow_list({
  persona_id?: string,            // metadata.persona filter
  trigger_kind?: string,          // filter
  enabled_only?: boolean          // default true
}) -> { workflows: Workflow[] }

mcp__persona-wire__wire_workflow_fire({
  id?:    string,                 // 単発 fire (on_demand 用)
  event?: string,                 // event 名で一括 fire (on_event 用)
  persona_id?: string,            // event fire の scope 絞り
  dry_run?: boolean               // default false、 true = action skip
}) -> {
  fired: [{ id, result, status }],
  skipped: [{ id, reason }]
}

mcp__persona-wire__wire_workflow_delete({
  id_or_name: string
}) -> { deleted, id_or_name, kind: "node" }
```

`register` は内部で `wire_node_create(type="workflow", ...)` に等価変換 +
schema validation (trigger / action kind の discriminator チェック)。
`delete` は既存 `wire_node_delete` と同じ shape。

---

## 6. wire_update (sibling)

`wire_update` は **cross-ref 自動維持** 用の独立 Tool。 P5 単独 land では
optional carry、 ただし `trigger.kind = metadata_changed` の land path として
本 doc で輪郭だけ確定:

```
mcp__persona-wire__wire_update({
  id: string,                     // 対象 Node id
  metadata_patch: object,         // shallow merge
  cascade?: boolean               // default false、 true = workflow fire 連鎖
}) -> { updated, fired_workflows }
```

cascade=true 時、 `trigger.kind = metadata_changed` かつ `watch_spec` が
本 Node を含む workflow を自動 fire (= chain reaction)。 無限 loop 抑止は
1 invoke per Node per call で fence (= 同 invoke 内で同 Node を 2 度更新
しても 1 回しか fire しない)。

---

## 7. UC mapping (onboarding §6b との対応)

| UC | 設計上の表現 |
|---|---|
| UC1 *session-close review* | `trigger: {kind: on_event, event: "session_close"}` + `action: {kind: emit_projection, projection_names: ["review_pending"]}`。 close Skill が `wire_workflow_fire({event: "session_close", persona_id})` |
| UC2 *wake-time pending list* | 同上、 `event: "session_wake"` |
| UC3 *stale node surfacing* | `trigger: {kind: cron, cron_spec: "0 9 * * *"}` + `action: emit_projection` → daemon land 後に自走、 land 前は cron Skill が `wire_workflow_fire({id})` |
| (new) *cross-ref refresh* | `trigger: {kind: metadata_changed, watch_spec: <A>}` + `action: set_metadata({target_spec: <B>, patch: {review_on_close: true}})` |

= caller が Skill / Hook / cron で **`wire_workflow_fire` を 1 行呼ぶだけ**
で済む。 caller 側に "どの projection を引くか" のハードコード不要。

---

## 8. Visualization / observability

Workflow は **graph 上の Node** なので既存 surface でそのまま観察できる:

- `wire_query({spec: {TypeIs: "workflow_def"}})` で全 workflow list
- `wire_doctor` に **Orphan workflow 検出** 追加 (= action.projection_names
  が存在しない、 watch_spec が空集合等) は別 issue 領分
- `wire_workflow_list` は read 軸の syntactic sugar (= 内部で `wire_query` を
  叩く)

= 新規 store / 新規 view を作らず、 既存 storage primitive で完結。

---

## 9. Out of scope (P5 範囲外、 明示 carry)

- **Daemon 実装** (cron tick / lifecycle scan / output channel emit) → P3a / P3c
- **mailbox 以外の output channel** (slack / webhook / file write) → P3b 領分
- **複雑な action chain / DAG / conditional branching** → V2 carry、 P5 は 1
  workflow = 1 action 固定
- **Workflow の history 永続化** (`last_fired_at` 以外の trace) → mini-app
  side で別 table、 wire 内には持たない
- **retry / backoff / failure policy** → daemon land 後 carry
- **動的 trigger 条件** (`every N events` 等) → V2

---

## 10. Implementation order (P5 内 sub-step)

| step | scope | rationale |
|---|---|---|
| P5-a | Node type `workflow` を type_registry に追加 + register/list/delete Tool (= on_demand のみ動く) | 既存 primitive で 90% 賄える、 ~150 行規模 |
| P5-b | `wire_workflow_fire` で `on_event` / `on_demand` の 2 trigger × `emit_projection` / `no_op` の 2 action を land | UC1-3 のうち UC1/UC2 が即動く |
| P5-c | `wire_update` + `trigger.kind = metadata_changed` + `action.kind = set_metadata` | cross-ref UC が動く、 cascade fence の test 重要 |
| P5-d | `cron` trigger を data として受理 (daemon 未着地時は silent skip)、 doc 反映 | P3a land 時に caller 側変更ゼロで自走化 |
| P5-e (optional) | `fire_mailbox` action (Adapter write-path)。 mini-app side との contract 整理が要るので必要に応じて | P3a/P3c land 後の方が自然 |

P5-a + P5-b で onboarding §6b の UC1/UC2 が caller 側 1 行で書ける状態に
なる = 最小 useful land 単位。

---

## 11. Open questions

1. **`event` 名前空間**: caller が任意文字列を渡せる free-form にするか、
   reserved 名 (`session_close` / `session_wake` / `mailbox_received` 等) を
   先に定義するか。 → free-form で start、 慣習が固まれば reserved 化を carry
2. **同 event に複数 workflow が hit した場合の fire 順序**: 登録順? metadata
   `priority` field? → 登録順 (= 決定性確保)、 priority field は V2 carry
3. **`dry_run`** は action ごとに skip 仕様を別途定義する必要あり (=
   `set_metadata` は write skip、 `emit_projection` は render するが返却のみ
   とする等) → 仕様確定は P5-b 着手時
4. **`wire_workflow_list` を別 Tool として切るか、 `wire_query` で済ますか**:
   syntactic sugar の価値次第 → caller 体験を優先して別 Tool で start

---

## 12. References

- 上位概念: `concept-2026-06-14.md` §7 Phase plan (P5 / P3a / P3c)
- UC source: `onboarding.md §6b` Loop / review / update-check trigger pattern
- 既存 sibling: `wire-query-spec.md` (Specification AST、 trigger.watch_spec で
  再利用)
- 既存 primitive: `Specification` / `NamedProjection` / Layer 6 Adapter

---

これは叩き台です。 修正があれば指摘してください。 全然違っていれば破棄します。
