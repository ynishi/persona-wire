//! `Workflow` Entity — trigger-driven autonomous entity (surrogate key).
//!
//! Sibling of `Wiring`; both are owned directly by `ContextWiring`. Has its
//! own id + `persona_id`, fires via trigger (`on_event` / `on_schedule`),
//! executes action (`emit_projection` etc.), and carries mutable state
//! (`enabled: bool`).
//!
//! Step B: skeleton. Rich impl lands LAST in Step C (most complex entity —
//! trigger / action / fire / cross-persona scope / `workflow_def` Node
//! borrow / doctor 5 probes / naming helper).
