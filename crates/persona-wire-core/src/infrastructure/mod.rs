//! Infrastructure layer — drives Storage and Rendering as adapters.
//!
//! - [`storage`]   — SQLite (rusqlite) adapter implementing graph persistence
//! - [`rendering`] — Prompt / Markdown / JSON / ASCII rendering adapter
//! - [`adapter`]   — Layer 6 SoT Adapter (mini-app / file / outline / ...) で fresh fetch

pub mod adapter;
pub mod rendering;
pub mod storage;
