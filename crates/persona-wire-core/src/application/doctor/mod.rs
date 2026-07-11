//! wire_doctor module — Finding-driven 2-axis (graph / workflow) health
//! diagnostic. Each Probe emits Severity-tagged Finding(s); [`run`] aggregates
//! a verdict (HEALTHY / DEGRADED / BROKEN) and renders a Markdown report.
//! Two modes: Full (`persona_id = None`) / Persona-scoped (`Some(id)`).
//!
//! Entry: [`run`] is a thin transport. Internally it drives the probe
//! registry → FindingSink → render pipeline.

pub mod finding;
pub mod probe;
pub mod probes;
pub mod registry;
pub mod render;
#[cfg(test)]
pub(crate) mod test_helpers;

use crate::application::plugin_registry::PluginRegistry;
use crate::domain::error::WireResult;
use crate::infrastructure::storage::SqliteStorage;

pub use finding::{Axis, Finding, Kind, Location, Severity};
pub use probe::{FindingSink, Probe, ProbeCtx};
pub use render::{aggregate_verdict, Verdict};

/// design §3: Full mode (`persona_id = None`) / Persona-scoped mode (`Some(id)`)。
/// 戻り値は Markdown report (design §8 layout + adapter-filter-if Phase 1
/// `## Adapters` 節)。 `registry` は登録 adapter の scheme + filter capability
/// 一覧 ([`PluginRegistry::describe`]) を `## Adapters` 節に反映するために使う。
pub fn run(
    storage: &SqliteStorage,
    persona_id: Option<String>,
    registry: &PluginRegistry,
) -> WireResult<String> {
    let ctx = ProbeCtx {
        storage,
        persona_filter: persona_id.clone(),
    };
    let mut sink = FindingSink::new();
    for probe in self::registry::default() {
        probe.scan(&ctx, &mut sink)?;
    }
    let mut report = render::to_markdown(persona_id.as_deref(), sink.into_vec());
    report.push_str(&render::render_adapters(&registry.describe()));
    Ok(report)
}
