# persona-wire-core::domain::graph::compute

Compute primitive — traversal + execution + constraint eval.

Handles 3 axes within one primitive:
1. Traversal     — BFS graph walk bounded by `max_depth` (P1: 1 hop default)
2. Execution     — workflow run / step transition (P5 carry)
3. Constraint    — bulk eval of constraint-kind edges (P3 carry)

## Functions

- `traverse` — BFS traverse from `start`, visiting each reachable node up to `max_depth`

## Types

- `TraversalResult` — (no documentation)

