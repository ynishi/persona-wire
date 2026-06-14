//! Infrastructure layer — drives Storage and Rendering as adapters.
//!
//! - [`storage`]   — SQLite (rusqlite) adapter implementing graph persistence
//! - [`rendering`] — Prompt / Markdown / JSON / ASCII rendering adapter

pub mod rendering;
pub mod storage;
