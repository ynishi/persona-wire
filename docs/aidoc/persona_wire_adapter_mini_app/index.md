# persona-wire-adapter-mini-app 0.14.0

persona-wire Adapter for mini-app SoT (scheme `mini-app://`).

Adapter type: [`MiniAppAdapter`] — zero-sized, wire it directly
into a [`persona_wire_core::application::plugin_registry::PluginRegistry`]
via `.with_adapter(MiniAppAdapter)`.

P3b roadmap deliverable: external adapter crate split out from
`persona-wire-core`. Consumers wire this adapter into their `PluginRegistry`
by chaining `.with_adapter(MiniAppAdapter)` on top of
[`persona_wire_core::application::plugin_registry::PluginRegistry::default_builder_for_wire`].

Supported URI:
`mini-app://<table>[?scope=user|<project-name>&root=<dir>&alias=<name>&<k>=<v>*&limit=<n>]`

- `scope` (`user` → `AliasScope::User` / 任意 project identifier → `AliasScope::Project`、
  省略時 = global storage (User scope) → per-table `_aliases` fallback)
- `root` (= 物理 dir 上書き、 `scope=<project-name>` 時は必須)
- `alias` (= global `_global.db` 内 `_global_aliases` (mini-app v0.12.1+ default) +
  legacy per-table `_aliases` (backward compat) 双方解決対応)
- `limit` (= list 上限 override)

render / parse / list は SDK (`mini_app_core::alias_run::execute_alias_run`) に完全委譲、
wire は filter / MiniJinja / ListFilter 意味論を一切解釈しない。

