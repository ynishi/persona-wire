//! Domain Ports — outbound (Driven) trait contracts owned by the Domain.
//!
//! Hexagonal Architecture (Cockburn 2005) では「出力の方向性 (= 何をどう外に出すか)」
//! を Domain が宣言し、 技術依存の実装は Adapter (Infrastructure) が担う。 本 module
//! はその Driven Port を集約する。
//!
//! Adapter (impl) は [`crate::infrastructure`] 配下、 default 同梱 adapter は
//! [`crate::infrastructure::projection`]。

pub mod projection_renderer;

pub use projection_renderer::{ProjectionInput, ProjectionRenderer};
