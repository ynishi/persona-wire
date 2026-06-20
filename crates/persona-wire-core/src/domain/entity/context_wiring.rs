//! `ContextWiring` Aggregate Root — persona's context-bearing wiring set.
//!
//! Consistency boundary. Owns `Vec<Wiring>` and `Vec<Workflow>` directly
//! (Vernon Rule 2 applied — persona-wire scale stays small per persona, so
//! transactional consistency fits inside one aggregate).
//!
//! Invariant: every owned `Wiring` / `Workflow` belongs to `persona_id`.
//!
//! Step B: skeleton. Rich impl lands in Step C.
