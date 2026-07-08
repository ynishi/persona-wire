# persona-wire-core::application::doctor::probes::graph_orphan_node

graph.orphan_node — in/out edge ゼロ + 自己参照なしの node を warn で emit。

design.md §6 entry。 既存 `use_cases::graph_scan_summary` の orphan 判定
ロジック (`is_self_attached_wiring`) を node 単位で再走査する。

## Types

- `GraphOrphanNode` — (no documentation)

