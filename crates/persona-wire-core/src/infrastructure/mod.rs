//! Infrastructure layer — drives Storage and Rendering as adapters.
//!
//! - [`storage`]   — SQLite (rusqlite) adapter implementing graph persistence
//! - [`rendering`] — Prompt / Markdown / JSON / ASCII rendering adapter (thin wrapper over `template`)
//! - [`template`]  — Template Engine Plugin trait (`HandlebarsEngine` default impl)
//! - [`adapter`]   — Layer 6 SoT Adapter Plugin trait (`FileAdapter`; mini-app は外部 crate `persona-wire-adapter-mini-app`)

pub mod adapter;
pub mod rendering;
pub mod storage;
pub mod template;
