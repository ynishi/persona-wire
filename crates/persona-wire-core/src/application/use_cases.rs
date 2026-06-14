//! Use cases — orchestration of Domain + Infrastructure for pnet_* flows.
//!
//! Each use case is a thin pure function (or struct) that takes ports
//! (storage / rendering) as deps and calls Domain primitives.

use crate::domain::error::WireResult;

pub struct PnetInitInput {
    pub persona_id: String,
}

pub struct PnetInitOutput {
    pub context_toc_json: serde_json::Value,
    pub warnings: Vec<String>,
}

pub fn pnet_init(_input: PnetInitInput) -> WireResult<PnetInitOutput> {
    // TODO(P1): chain wire_project calls against registered named projections.
    Ok(PnetInitOutput {
        context_toc_json: serde_json::json!({}),
        warnings: Vec::new(),
    })
}

pub struct PnetCloseInput {
    pub persona_id: String,
}

pub struct PnetCloseOutput {
    pub report_markdown: String,
}

pub fn pnet_close(_input: PnetCloseInput) -> WireResult<PnetCloseOutput> {
    // TODO(P1): aggregate lifecycle scan + emit report.
    Ok(PnetCloseOutput {
        report_markdown: String::new(),
    })
}
