//! Domain Entity Layer — persona-wire vocabulary as first-class entities.
//!
//! Sits on top of [`crate::domain::graph`] (Math backend SDK, the
//! tenant-agnostic graph primitives) and below the application layer.
//!
//! # Layer split
//!
//! - **Math backend** (`crate::domain::graph`) — open-vocabulary graph
//!   primitives (Node / Edge / Specification / CRUD / Compute / Constraint /
//!   AutoVersion / Repository). Tenant-agnostic, persona-agnostic. Owns no
//!   domain knowledge of personas, slots, sources, or projections.
//! - **Domain Entity** (this module) — persona-wire's first-class vocabulary
//!   (Persona / Slot / Source / Projection / Wiring / Workflow). Uses the
//!   Math backend as a persistence SDK; the backend stays unaware of these
//!   entities.
//!
//! # Composition
//!
//! ```text
//! Projection (Aggregate Root)
//!   — rendering intent (template + spec_ref + plugin dispatch).
//!   — the Wire's external OUT surface.
//!
//! Wiring (Entity)
//!   ├ persona_id: PersonaId      (natural composite key)
//!   ├ slot:       Slot           (natural composite key)
//!   ├ source:     Source         (directly owned)
//!   └ projection_ref: Option<ProjectionName>  (identity ref, Vernon Rule 3)
//!
//! Workflow (Entity)
//!   ├ id:         WorkflowId     (surrogate)
//!   ├ persona_id: Option<PersonaId>
//!   ├ trigger:    Trigger        (OnDemand | OnEvent)
//!   ├ action:     Action         (NoOp | EmitProjection { slots: Vec<Slot> })
//!   └ enabled:    bool
//! ```
//!
//! # Surface policy: Wire's OUT is `Projection`
//!
//! `Projection` is the only rendering surface exposed to application /
//! MCP callers. `Wiring` and `Workflow` are **internal vocabulary** —
//! they are not re-exported at this module root. Application code reading
//! raw `Wiring` / `Workflow` would bypass the rendering layer, so direct
//! access goes through the fully qualified
//! `crate::domain::entity::{wiring, workflow}` paths and is reserved for
//! entity-internal composition or a future Aggregate Root.
//!
//! # Persistence
//!
//! `Wiring` and `Workflow` are persisted through the existing Math backend
//! Repository (`Node` CRUD via [`crate::domain::graph`]). They do **not**
//! introduce a dedicated Registry / DTO / table. The Projection-specific
//! `ProjectionRegistry` (separate table + DTO + Mapper) is intentional —
//! Projection is the externally referenced rendering Aggregate Root, while
//! Wiring / Workflow live behind it as configuration data the Repository
//! pattern already handles.

pub mod bundle;
pub mod context_wiring;
pub mod persona_id;
pub mod projection;
pub mod slot;
pub mod source;
pub mod wiring;
pub mod workflow;

pub use bundle::{
    Bundle, BundleId, BundleInstallReport, BundleName, BundleRef, BundleVersion, ConflictMode,
    ErrorItem, InstalledItem, SkippedItem,
};
pub use persona_id::PersonaId;
pub use projection::{
    PluginDispatch, Projection, ProjectionName, ProjectionTemplate, SpecName, SpecRef, TargetForm,
};
pub use slot::Slot;
pub use source::Source;

// `Wiring` / `Workflow` are intentionally NOT re-exported here. See the
// "Surface policy" section in the module docstring above.
