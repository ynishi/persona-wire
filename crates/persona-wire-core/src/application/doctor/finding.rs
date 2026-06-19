//! Finding — wire_doctor が emit する 1 件の NG 観察。
//!
//! design.md §4 / §5 / §6 / §7 に対応する core types。
//! Kind は内部 closed Enum (新 kind 追加で variant + Probe 同時編集を強制)。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warn,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warn => "warn",
            Severity::Info => "info",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Graph,
    Workflow,
}

impl Axis {
    pub fn as_str(self) -> &'static str {
        match self {
            Axis::Graph => "graph",
            Axis::Workflow => "workflow",
        }
    }
}

/// `kind` enum: closed set. 1 variant = 1 Probe が default。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    GraphOrphanNode,
    GraphDanglingEdge,
    GraphEdgesZero,
    GraphSpecNoHit,
    GraphProjectionEmptyRender,
    WorkflowEmitTargetUnregistered,
    WorkflowTriggerInfraMissing,
    WorkflowDuplicateTrigger,
    WorkflowPersonaNoWorkflow,
    WorkflowLastFireFailed,
    WorkflowDisabled,
}

impl Kind {
    /// Markdown 出力時の literal (`graph.orphan_node` 形式)。
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::GraphOrphanNode => "graph.orphan_node",
            Kind::GraphDanglingEdge => "graph.dangling_edge",
            Kind::GraphEdgesZero => "graph.edges_zero",
            Kind::GraphSpecNoHit => "graph.spec_no_hit",
            Kind::GraphProjectionEmptyRender => "graph.projection_empty_render",
            Kind::WorkflowEmitTargetUnregistered => "workflow.emit_target_unregistered",
            Kind::WorkflowTriggerInfraMissing => "workflow.trigger_infra_missing",
            Kind::WorkflowDuplicateTrigger => "workflow.duplicate_trigger",
            Kind::WorkflowPersonaNoWorkflow => "workflow.persona_no_workflow",
            Kind::WorkflowLastFireFailed => "workflow.last_fire_failed",
            Kind::WorkflowDisabled => "workflow.disabled",
        }
    }

    pub fn axis(self) -> Axis {
        match self {
            Kind::GraphOrphanNode
            | Kind::GraphDanglingEdge
            | Kind::GraphEdgesZero
            | Kind::GraphSpecNoHit
            | Kind::GraphProjectionEmptyRender => Axis::Graph,
            Kind::WorkflowEmitTargetUnregistered
            | Kind::WorkflowTriggerInfraMissing
            | Kind::WorkflowDuplicateTrigger
            | Kind::WorkflowPersonaNoWorkflow
            | Kind::WorkflowLastFireFailed
            | Kind::WorkflowDisabled => Axis::Workflow,
        }
    }

    /// design §6 / §7 表に従う default。 Probe 側で override 可能。
    pub fn default_severity(self) -> Severity {
        match self {
            Kind::GraphOrphanNode | Kind::GraphSpecNoHit => Severity::Warn,
            Kind::GraphDanglingEdge
            | Kind::GraphEdgesZero
            | Kind::GraphProjectionEmptyRender => Severity::Error,
            Kind::WorkflowEmitTargetUnregistered | Kind::WorkflowTriggerInfraMissing => {
                Severity::Error
            }
            Kind::WorkflowDuplicateTrigger
            | Kind::WorkflowPersonaNoWorkflow
            | Kind::WorkflowLastFireFailed => Severity::Warn,
            Kind::WorkflowDisabled => Severity::Info,
        }
    }
}

/// 発生場所 — 固有名詞で指差すための field bundle。 全 Optional。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Location {
    pub node_id: Option<String>,
    pub persona_id: Option<String>,
    pub workflow_id: Option<String>,
    pub edge: Option<(String, String)>, // (src, tgt)
    pub projection_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub axis: Axis,
    pub kind: Kind,
    pub location: Location,
    pub description: String,
    /// 修正方法。 推奨は MCP tool call literal (例: `mcp__persona-wire__wire_edge_delete(...)`)。
    pub fix: String,
}
