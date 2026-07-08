# persona-wire-core::domain::graph::specification

Specification primitive — first-class composable query object.

BP reference: Specification pattern (Evans / Fowler / Greg Young).
`Specification` is the **domain object** representing a query predicate;
Application layer holds a registry that stores composed Specifications by name.

## Types

- `Specification` — Specification — composable query predicate over the graph.

