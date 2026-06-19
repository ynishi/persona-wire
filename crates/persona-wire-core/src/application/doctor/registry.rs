//! Probe registry — default 全埋め込み。
//!
//! 新 Probe 追加時はここに 1 行 push する。 wire_doctor 本体 / lib.rs MCP 経路は触らない。

use crate::application::doctor::probe::Probe;
use crate::application::doctor::probes;

/// default 全埋め込み。 順序は最終 Markdown 出力順 (Graph 軸 → Workflow 軸)。
pub fn default() -> Vec<Box<dyn Probe>> {
    vec![
        // Graph axis
        Box::new(probes::GraphEdgesZero),
        Box::new(probes::GraphDanglingEdge),
        Box::new(probes::GraphOrphanNode),
        // TODO: GraphSpecNoHit / GraphProjectionEmptyRender (別 issue carry)
        // Workflow axis
        Box::new(probes::WorkflowEmitTargetUnregistered),
        Box::new(probes::WorkflowTriggerInfraMissing),
        Box::new(probes::WorkflowDuplicateTrigger),
        Box::new(probes::WorkflowPersonaNoWorkflow),
        Box::new(probes::WorkflowDisabled),
        // TODO: WorkflowLastFireFailed (schema 拡張前提、 別 issue carry)
    ]
}
