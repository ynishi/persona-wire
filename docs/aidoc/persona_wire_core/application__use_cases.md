# persona-wire-core::application::use_cases

Use cases — orchestration of Domain + Infrastructure for wire_* flows.

## Functions

- `graph_scan_summary` — Walk every node type and tally totals + orphan count. A node is counted as
- `wire_close` — Minimal lifecycle scan for the `/work-close` auto-call. P1 reports orphan
- `wire_context_get` — Walk one persona's consistency boundary and return a structured
- `wire_doctor` — Finding-driven 2-axis (graph / workflow) health diagnostic (design §3-§8).
- `wire_edge_delete` — Delete an edge by id.
- `wire_edges_create_batch` — Insert a batch of edges by iterating `insert_edge` 1 row at a time. Same
- `wire_fetch` — Raw adapter preview — routes a URI through the same `PluginRegistry`
- `wire_init` — Run every registered NamedProjection against the current graph and return
- `wire_node_delete` — Delete a node by id. Edges referencing the node (as src or tgt) are
- `wire_node_update` — Update a node's `metadata` in place.
- `wire_nodes_create_batch` — Insert a batch of nodes by iterating `insert_node` 1 row at a time. Stops
- `wire_projection_delete` — Delete a NamedProjection by ULID id or name.
- `wire_prompt_context` — 全 builtin slot (or projection_names で subset) を iterate し、 各 slot の
- `wire_query` — Ad-hoc query: evaluate `spec` (inline or by registered name) against the
- `wire_render` — Render a single registered NamedProjection by name. Counterpart to
- `wire_slot_delete` — Counterpart to [`wire_slot_register`] — removes the wiring node, the
- `wire_slot_register` — One-shot slot setup — a macro over the three registrations the onboarding
- `wire_spec_delete` — Delete a Specification by ULID id or name. Projections referencing it via
- `wire_workflow_fire` — Resolve the workflows that would fire for the given input. **Does not**
- `wire_workflow_list` — List registered Workflows (= Nodes of type `workflow_def`), with optional
- `wire_workflow_register` — Register a Workflow as a `workflow_def` Node. Routes through the

## Types

- `GraphScanSummary` — Shared graph health summary used by `wire_close` (persona-scoped report)
- `RenderedProjection` — (no documentation)
- `ResolvedFire` — A workflow resolved for firing, with its action descriptor surfaced so the
- `WireBatchOutput` — (no documentation)
- `WireCloseInput` — (no documentation)
- `WireCloseOutput` — (no documentation)
- `WireContextGetInput` — Input for `wire_context_get`. Just the persona scope.
- `WireContextGetOutput` — 1-call read view of a `ContextWiring` (per-persona Aggregate boundary).
- `WireDeleteInput` — (no documentation)
- `WireDeleteOutput` — (no documentation)
- `WireDoctorOutput` — Finding-driven 2-axis (graph / workflow) health diagnostic output.
- `WireEdgesCreateBatchInput` — (no documentation)
- `WireFetchInput` — Input for [`wire_fetch`] — either a raw `source_uri`, or a
- `WireFetchOutput` — (no documentation)
- `WireInitInput` — (no documentation)
- `WireInitOutput` — (no documentation)
- `WireNodeUpdateInput` — (no documentation)
- `WireNodeUpdateMode` — Merge strategy for `wire_node_update`. Mirrors RFC 7396 shallow merge for
- `WireNodeUpdateOutput` — (no documentation)
- `WireNodesCreateBatchInput` — (no documentation)
- `WirePromptContextInput` — (no documentation)
- `WirePromptContextOutput` — (no documentation)
- `WireQueryInput` — (no documentation)
- `WireQueryNode` — (no documentation)
- `WireQueryOutput` — (no documentation)
- `WireRenderInput` — (no documentation)
- `WireRenderOutput` — (no documentation)
- `WireSlotDeleteInput` — (no documentation)
- `WireSlotDeleteOutput` — (no documentation)
- `WireSlotRegisterInput` — Input for [`wire_slot_register`] — the minimal real information a slot
- `WireSlotRegisterOutput` — (no documentation)
- `WireWorkflowFireInput` — (no documentation)
- `WireWorkflowFireOutput` — (no documentation)
- `WireWorkflowListInput` — (no documentation)
- `WireWorkflowListOutput` — (no documentation)
- `WireWorkflowRegisterInput` — (no documentation)
- `WireWorkflowRegisterOutput` — (no documentation)
- `WiringSummary` — Application-layer summary of one `Wiring`. Carries only the fields a
- `WorkflowSummary` — (no documentation)

