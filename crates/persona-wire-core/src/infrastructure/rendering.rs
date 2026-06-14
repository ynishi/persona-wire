//! Rendering adapter — render extracted graph subsets into output forms.
//!
//! P1 scope: a minimal `{{path.to.field}}` substitution engine over JSON data.
//! Per concept-doc §6, the choice of a richer DSL (Lua / Tera) is deferred to
//! the project's "rich rendering" milestone; the minimal engine keeps wire
//! self-contained and avoids the mlua / tera dependency at the P1 boundary.

use crate::application::projection_registry::TargetForm;

/// Render `template` by substituting `{{key.path}}` occurrences with values
/// looked up from `data` (dotted JSON path, e.g. `{{user.name}}`).
///
/// Behaviour:
/// - Whitespace inside the braces is trimmed: `{{ a.b }}` ≡ `{{a.b}}`.
/// - Missing paths render as the empty string.
/// - Non-string values render via their JSON representation (no surrounding quotes for strings).
/// - `target_form` is currently informational; future variants (e.g. JSON-array
///   wrapping) will dispatch on this.
pub fn render(target_form: TargetForm, template: &str, data: &serde_json::Value) -> String {
    let _ = target_form;
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if let Some(end) = find_closing(bytes, i + 2) {
                let key = std::str::from_utf8(&bytes[i + 2..end]).unwrap_or("").trim();
                out.push_str(&lookup(data, key));
                i = end + 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn find_closing(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    while j + 1 < bytes.len() {
        if bytes[j] == b'}' && bytes[j + 1] == b'}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn lookup(data: &serde_json::Value, key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let mut current = data;
    for part in key.split('.') {
        match current.get(part) {
            Some(next) => current = next,
            None => return String::new(),
        }
    }
    match current {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn substitutes_simple_key() {
        let out = render(TargetForm::Prompt, "Hi, {{name}}!", &json!({"name": "shi"}));
        assert_eq!(out, "Hi, shi!");
    }

    #[test]
    fn substitutes_nested_path() {
        let out = render(
            TargetForm::Markdown,
            "Owner is {{owner.name}}.",
            &json!({"owner": {"name": "ytk"}}),
        );
        assert_eq!(out, "Owner is ytk.");
    }

    #[test]
    fn trims_inner_whitespace() {
        let out = render(TargetForm::Prompt, "X={{ a.b }}", &json!({"a": {"b": 7}}));
        assert_eq!(out, "X=7");
    }

    #[test]
    fn missing_path_renders_empty() {
        let out = render(TargetForm::Prompt, "[{{absent}}]", &json!({"name": "shi"}));
        assert_eq!(out, "[]");
    }

    #[test]
    fn passes_through_text_without_braces() {
        let out = render(TargetForm::Markdown, "plain text", &json!({}));
        assert_eq!(out, "plain text");
    }

    #[test]
    fn unmatched_open_brace_passes_through() {
        let out = render(TargetForm::Prompt, "{{ no_close", &json!({}));
        assert_eq!(out, "{{ no_close");
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
}
