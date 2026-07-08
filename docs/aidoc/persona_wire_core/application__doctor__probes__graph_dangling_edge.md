# persona-wire-core::application::doctor::probes::graph_dangling_edge

graph.dangling_edge — edge target に該当 node が存在しない (error)。

design.md §6 entry。 storage 側は FK NOT-NULL + `wire_node_delete` cascade
で dangling 状態を作らないため、 本 Probe は **defensive sensor** として
振る舞う (external DB drift / migration corruption / 直 SQL writes 等の
異常経路で混入した dangling edge を doctor の next scan で flag する)。

## Types

- `GraphDanglingEdge` — (no documentation)

