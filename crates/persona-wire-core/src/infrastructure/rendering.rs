//! Rendering adapter — render extracted graph subsets into output forms.
//!
//! Target forms: Prompt / Markdown / JSON / ASCII graph.
//! Template engine selection (Lua / Tera / minimal) is decided at P0.

use crate::application::projection_registry::TargetForm;

pub fn render(_target_form: TargetForm, _template: &str, _data: &serde_json::Value) -> String {
    // TODO(P0+): wire template engine after DSL decision.
    String::new()
}
