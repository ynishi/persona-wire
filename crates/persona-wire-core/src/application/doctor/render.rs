//! Finding → Markdown render + verdict 集約。
//!
//! design.md §5 (verdict) / §8 (output 形式) に対応。

use crate::application::doctor::finding::{Axis, Finding, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Healthy,
    Degraded,
    Broken,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Healthy => "HEALTHY",
            Verdict::Degraded => "DEGRADED",
            Verdict::Broken => "BROKEN",
        }
    }
}

/// design §5: error >= 1 → BROKEN / warn >= 1 → DEGRADED / 全 PASS → HEALTHY。
pub fn aggregate_verdict(findings: &[Finding]) -> Verdict {
    let mut has_error = false;
    let mut has_warn = false;
    for f in findings {
        match f.severity {
            Severity::Error => has_error = true,
            Severity::Warn => has_warn = true,
            Severity::Info => {}
        }
    }
    if has_error {
        Verdict::Broken
    } else if has_warn {
        Verdict::Degraded
    } else {
        Verdict::Healthy
    }
}

pub fn to_markdown(persona_filter: Option<&str>, findings: Vec<Finding>) -> String {
    let verdict = aggregate_verdict(&findings);
    let scope = match persona_filter {
        Some(id) => format!("persona:{id}"),
        None => "full".to_string(),
    };

    let mut error_n = 0_usize;
    let mut warn_n = 0_usize;
    let mut info_n = 0_usize;
    for f in &findings {
        match f.severity {
            Severity::Error => error_n += 1,
            Severity::Warn => warn_n += 1,
            Severity::Info => info_n += 1,
        }
    }

    let mut out = String::new();
    out.push_str("# wire_doctor report\n\n");
    out.push_str(&format!("scope: {scope}\n"));
    out.push_str(&format!("verdict: {}\n", verdict.as_str()));
    out.push_str(&format!(
        "summary: error={error_n} warn={warn_n} info={info_n}\n\n"
    ));

    out.push_str(&render_axis(Axis::Graph, &findings));
    out.push_str(&render_axis(Axis::Workflow, &findings));

    out
}

fn render_axis(axis: Axis, findings: &[Finding]) -> String {
    let subset: Vec<&Finding> = findings.iter().filter(|f| f.axis == axis).collect();
    let title = match axis {
        Axis::Graph => "Graph axis",
        Axis::Workflow => "Workflow axis",
    };
    let mut out = format!("## {title} ({} findings)\n\n", subset.len());
    if subset.is_empty() {
        out.push_str("_(no findings)_\n\n");
        return out;
    }
    for f in subset {
        out.push_str(&render_finding(f));
    }
    out
}

fn render_finding(f: &Finding) -> String {
    let mut out = String::new();
    let head_id = primary_id(f);
    out.push_str(&format!(
        "### [{severity}] {kind} — {head}\n",
        severity = f.severity.as_str(),
        kind = f.kind.as_str(),
        head = head_id,
    ));
    out.push_str(&format!("- location: {}\n", render_location(f)));
    out.push_str(&format!("- description: {}\n", f.description));
    out.push_str(&format!("- fix: {}\n\n", f.fix));
    out
}

fn primary_id(f: &Finding) -> String {
    if let Some(ref n) = f.location.node_id {
        return format!("node `{n}`");
    }
    if let Some(ref w) = f.location.workflow_id {
        return format!("workflow `{w}`");
    }
    if let Some((ref s, ref t)) = f.location.edge {
        return format!("edge `{s}` → `{t}`");
    }
    if let Some(ref p) = f.location.projection_name {
        return format!("projection `{p}`");
    }
    if let Some(ref p) = f.location.persona_id {
        return format!("persona `{p}`");
    }
    "(unspecified)".to_string()
}

fn render_location(f: &Finding) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref n) = f.location.node_id {
        parts.push(format!("node_id=`{n}`"));
    }
    if let Some(ref p) = f.location.persona_id {
        parts.push(format!("persona_id=`{p}`"));
    }
    if let Some(ref w) = f.location.workflow_id {
        parts.push(format!("workflow_id=`{w}`"));
    }
    if let Some((ref s, ref t)) = f.location.edge {
        parts.push(format!("edge=`{s}`→`{t}`"));
    }
    if let Some(ref p) = f.location.projection_name {
        parts.push(format!("projection=`{p}`"));
    }
    if parts.is_empty() {
        "(unspecified)".to_string()
    } else {
        parts.join(" ")
    }
}
