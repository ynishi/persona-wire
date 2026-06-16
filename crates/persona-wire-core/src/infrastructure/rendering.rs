//! Rendering adapter — render extracted graph subsets into output forms.
//!
//! Engine: `handlebars` (Mustache superset) — supports section iteration
//! (`{{#each list}}…{{/each}}`), conditionals (`{{#if cond}}…{{/if}}`),
//! dotted path lookup (`{{a.b.c}}`), and the existing scalar substitution
//! syntax (backward-compatible with the P1 minimal engine).
//!
//! HTML-escape is **disabled** globally: wire emits markdown / prompt /
//! json / ascii, none of which want HTML entity encoding (`<` → `&lt;`).

use crate::application::projection_registry::TargetForm;
use handlebars::{no_escape, Handlebars};

/// Render `template` against `data` using a handlebars engine.
///
/// Behaviour:
/// - Scalar substitution: `{{key.path}}` looks up dotted JSON paths.
/// - Section iteration: `{{#each list}}{{this.field}}{{/each}}` walks arrays.
/// - Conditionals: `{{#if cond}}…{{/if}}` evaluates truthiness.
/// - Missing paths render as the empty string (handlebars default).
/// - HTML escape is OFF (markdown/prompt/json/ascii outputs preserved verbatim).
/// - On template parse / render error, returns a `{{render-error: <msg>}}`
///   prefix followed by the raw template literal so callers can diagnose
///   syntax issues at-a-glance (never panics).
/// - `target_form` is currently informational; future variants (e.g. JSON-array
///   wrapping) will dispatch on this.
pub fn render(target_form: TargetForm, template: &str, data: &serde_json::Value) -> String {
    let _ = target_form;
    let mut hb = Handlebars::new();
    hb.register_escape_fn(no_escape);
    hb.set_strict_mode(false);
    match hb.render_template(template, data) {
        Ok(s) => s,
        Err(e) => format!("{{{{render-error: {}}}}} {}", e, template),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn substitutes_simple_key() {
        let out = render(
            TargetForm::Prompt,
            "Hi, {{name}}!",
            &json!({"name": "alpha"}),
        );
        assert_eq!(out, "Hi, alpha!");
    }

    #[test]
    fn substitutes_nested_path() {
        let out = render(
            TargetForm::Markdown,
            "Owner is {{owner.name}}.",
            &json!({"owner": {"name": "user_a"}}),
        );
        assert_eq!(out, "Owner is user_a.");
    }

    #[test]
    fn trims_inner_whitespace() {
        let out = render(TargetForm::Prompt, "X={{ a.b }}", &json!({"a": {"b": 7}}));
        assert_eq!(out, "X=7");
    }

    #[test]
    fn missing_path_renders_empty() {
        let out = render(
            TargetForm::Prompt,
            "[{{absent}}]",
            &json!({"name": "alpha"}),
        );
        assert_eq!(out, "[]");
    }

    #[test]
    fn passes_through_text_without_braces() {
        let out = render(TargetForm::Markdown, "plain text", &json!({}));
        assert_eq!(out, "plain text");
    }

    #[test]
    fn unmatched_open_brace_returns_visible_error() {
        let out = render(TargetForm::Prompt, "{{ no_close", &json!({}));
        // handlebars parse error → visible {{render-error: ...}} prefix.
        assert!(out.starts_with("{{render-error:"));
        assert!(out.contains("{{ no_close"));
    }

    #[test]
    fn number_renders_as_json_number() {
        let out = render(TargetForm::Json, "n={{x}}", &json!({"x": 42}));
        assert_eq!(out, "n=42");
    }

    #[test]
    fn null_renders_as_empty() {
        let out = render(
            TargetForm::Prompt,
            "v=[{{x}}]",
            &json!({"x": serde_json::Value::Null}),
        );
        assert_eq!(out, "v=[]");
    }

    #[test]
    fn multiple_substitutions_in_one_template() {
        let out = render(
            TargetForm::Prompt,
            "{{a}}-{{b}}-{{c}}",
            &json!({"a": "x", "b": "y", "c": "z"}),
        );
        assert_eq!(out, "x-y-z");
    }

    #[test]
    fn each_section_iterates_list() {
        let out = render(
            TargetForm::Markdown,
            "{{#each nodes}}- {{this.id}}\n{{/each}}",
            &json!({"nodes": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}),
        );
        assert_eq!(out, "- a\n- b\n- c\n");
    }

    #[test]
    fn each_section_with_nested_metadata_lookup() {
        let out = render(
            TargetForm::Markdown,
            "{{#each nodes}}- {{this.id}}: {{this.metadata.label}}\n{{/each}}",
            &json!({
                "nodes": [
                    {"id": "a", "metadata": {"label": "Alpha"}},
                    {"id": "b", "metadata": {"label": "Beta"}}
                ]
            }),
        );
        assert_eq!(out, "- a: Alpha\n- b: Beta\n");
    }

    #[test]
    fn if_section_evaluates_truthiness() {
        let out = render(
            TargetForm::Prompt,
            "{{#if has_carry}}carry present{{else}}no carry{{/if}}",
            &json!({"has_carry": true}),
        );
        assert_eq!(out, "carry present");
    }

    #[test]
    fn html_escape_is_disabled_for_markdown() {
        // markdown / prompt 出力で `<` `>` を `&lt;` `&gt;` にエンコードしない
        let out = render(
            TargetForm::Markdown,
            "tag: {{tag}}",
            &json!({"tag": "<div>"}),
        );
        assert_eq!(out, "tag: <div>");
    }

    #[test]
    fn empty_each_section_renders_empty() {
        let out = render(
            TargetForm::Markdown,
            "[{{#each nodes}}- {{this.id}}\n{{/each}}]",
            &json!({"nodes": []}),
        );
        assert_eq!(out, "[]");
    }
}
