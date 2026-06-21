//! persona-wire Adapter for Obsidian vault (scheme `obsidian://`).
//!
//! Reads Markdown notes from a local Obsidian vault directory, extracts
//! YAML / TOML frontmatter via [`gray_matter`], and returns structured JSON.
//!
//! ## URI form
//!
//! ```text
//! obsidian:///<vault-root>/<note>[?frontmatter={on|off}&links={off|edge}]
//! ```
//!
//! - `<vault-root>` — path to the Obsidian vault directory (absolute or `~/`-prefixed)
//! - `<note>` — note file name (relative to vault root)
//! - `?frontmatter` = `on` (default) | `off`
//! - `?links` = `off` (default) | `edge` (returns `wiki_links` array)
//!
//! ## Return shape
//!
//! ```json
//! {
//!   "vault_path": "<absolute vault root>",
//!   "note_path": "<note filename relative to vault>",
//!   "frontmatter": { ... } | null,
//!   "body": "<markdown body without frontmatter fence>",
//!   "wiki_links": [{"target": "Note A", "raw": "[[Note A]]"}]
//! }
//! ```
//!
//! `wiki_links` is only present when `?links=edge` is specified.

use std::path::PathBuf;

use async_trait::async_trait;
use gray_matter::{engine::YAML, Matter};
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};
use serde::Serialize;
use serde_json::{json, Value};

// ── Public Adapter struct ─────────────────────────────────────────────────────

/// persona-wire Adapter for Obsidian vault directories.
///
/// scheme literal: `"obsidian"`.
pub struct ObsidianAdapter;

// ── Internal URI spec ─────────────────────────────────────────────────────────

struct ObsidianUriSpec {
    /// Absolute path to the vault root directory.
    vault_root: PathBuf,
    /// Path of the note file, relative to `vault_root`.
    note_path: PathBuf,
    /// Whether to expand frontmatter (default: `true`).
    expand_frontmatter: bool,
    /// Whether to extract wiki-links and include them in the return JSON.
    extract_wiki_links: bool,
}

// ── WikiLink types ────────────────────────────────────────────────────────────

/// A single Obsidian wiki-link extracted from a Markdown body.
///
/// - `[[Target]]` → `{ target: "Target", raw: "[[Target]]" }`
/// - `[[Target|Alias]]` → `{ target: "Target", alias: "Alias", raw: "[[Target|Alias]]" }`
#[derive(Debug, Serialize)]
struct WikiLink {
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
    raw: String,
}

// ── URI parse helper ──────────────────────────────────────────────────────────

/// Parse an `obsidian:///<vault>/<note>[?query]` URI.
///
/// Mirrors the `parse_sqlite_uri` pattern from `persona-wire-adapter-sqlite-x`:
/// `strip_prefix("obsidian://")` then `split_once('?')` to separate path and query.
fn parse_obsidian_uri(source_uri: &str) -> WireResult<ObsidianUriSpec> {
    let rest = source_uri
        .strip_prefix("obsidian://")
        .ok_or_else(|| WireError::Storage(format!("obsidian adapter: bad uri: {source_uri}")))?;

    // Split query string from path.
    let (path_part, query_part) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };

    if path_part.is_empty() {
        return Err(WireError::Storage(format!(
            "obsidian adapter: missing path in {source_uri}"
        )));
    }

    // Decompose full path into vault_root (parent dir) and note_path (filename).
    // For `obsidian:///tmp/vault/note.md`:
    //   path_part   = "/tmp/vault/note.md"
    //   raw_vault   = "/tmp/vault"
    //   note_name   = "note.md"
    let full = PathBuf::from(path_part);

    let note_name = full
        .file_name()
        .ok_or_else(|| {
            WireError::Storage(format!(
                "obsidian adapter: missing note filename in {source_uri}"
            ))
        })?
        .to_string_lossy()
        .into_owned();

    let raw_vault = full
        .parent()
        .ok_or_else(|| {
            WireError::Storage(format!(
                "obsidian adapter: cannot derive vault root from {source_uri}"
            ))
        })?
        .to_string_lossy()
        .into_owned();

    let vault_root = expand_path(&raw_vault)?;

    // Parse query parameters (forward-compatible: unknown keys silently ignored).
    let mut expand_frontmatter = true;
    let mut extract_wiki_links = false;
    if let Some(qs) = query_part {
        for pair in qs.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => continue,
            };
            match k {
                "frontmatter" => expand_frontmatter = v == "on",
                "links" => extract_wiki_links = v == "edge",
                _ => {}
            }
        }
    }

    Ok(ObsidianUriSpec {
        vault_root,
        note_path: PathBuf::from(note_name),
        expand_frontmatter,
        extract_wiki_links,
    })
}

/// Expand a `~/`-prefixed path using the `HOME` environment variable.
///
/// Mirrors the `expand_path` helper from `persona-wire-adapter-sqlite-x`.
fn expand_path(raw: &str) -> WireResult<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("obsidian adapter: HOME unset".to_string()))?;
        Ok(PathBuf::from(home).join(rest))
    } else if raw == "~" {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("obsidian adapter: HOME unset".to_string()))?;
        Ok(PathBuf::from(home))
    } else {
        Ok(PathBuf::from(raw))
    }
}

// ── Frontmatter parse helper ──────────────────────────────────────────────────

/// Convert a `gray_matter::Pod` value into a `serde_json::Value`.
///
/// `Pod` does not implement `serde::Serialize` or `From<Pod> for Value`, so
/// the conversion is done manually by matching each enum variant.
fn pod_to_json(pod: gray_matter::Pod) -> Value {
    use gray_matter::Pod;
    match pod {
        Pod::Null => Value::Null,
        Pod::Boolean(b) => Value::Bool(b),
        Pod::Integer(i) => Value::Number(i.into()),
        Pod::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Pod::String(s) => Value::String(s),
        Pod::Array(arr) => Value::Array(arr.into_iter().map(pod_to_json).collect()),
        Pod::Hash(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, pod_to_json(v))).collect())
        }
    }
}

/// Convert a `toml::Value` into a `serde_json::Value`.
///
/// Used for TOML frontmatter (`+++` delimiters) that is parsed manually via the
/// `toml` crate, because gray_matter 0.2.x TOML engine does not recognise `+++`
/// as a valid frontmatter delimiter.
fn toml_value_to_json(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Array(arr) => Value::Array(arr.into_iter().map(toml_value_to_json).collect()),
        toml::Value::Table(t) => Value::Object(
            t.into_iter()
                .map(|(k, v)| (k, toml_value_to_json(v)))
                .collect(),
        ),
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
    }
}

/// Extract frontmatter and body from raw Markdown content.
///
/// Detects frontmatter type by prefix:
/// - `---` → YAML (most common Obsidian frontmatter form) — parsed via `gray_matter`
/// - `+++` → TOML — parsed manually via the `toml` crate (gray_matter 0.2.x TOML
///   engine does not recognise `+++` as a delimiter)
/// - otherwise → no frontmatter, body is the entire file
///
/// Returns `(frontmatter_json, body_string)`.
fn parse_frontmatter(raw: &str) -> (Value, String) {
    if raw.starts_with("---") {
        let result = Matter::<YAML>::new().parse(raw);
        let fm = result.data.map(pod_to_json).unwrap_or(Value::Null);
        (fm, result.content)
    } else if raw.starts_with("+++") {
        // Manually extract content between +++ delimiters.
        let after_open = raw.strip_prefix("+++").unwrap_or(raw);
        let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
        if let Some(close_pos) = after_open.find("\n+++") {
            let fm_str = &after_open[..close_pos];
            let body_raw = &after_open[close_pos + 4..]; // skip "\n+++"
            let body = body_raw.strip_prefix('\n').unwrap_or(body_raw);
            let fm = toml::from_str::<toml::Value>(fm_str)
                .ok()
                .map(toml_value_to_json)
                .unwrap_or(Value::Null);
            (fm, body.to_string())
        } else {
            (Value::Null, raw.to_string())
        }
    } else {
        (Value::Null, raw.to_string())
    }
}

// ── Wiki-link extraction helper ───────────────────────────────────────────────

/// Extract Obsidian wiki-links from a Markdown body string.
///
/// Scans line by line, tracking fenced code blocks (` ``` ` / `~~~`) and
/// skipping inline code spans (single `` ` ``).  Within regular text, looks for
/// `[[Target]]` and `[[Target|Alias]]` patterns.
///
/// Rules:
/// - Lines inside a fenced code block are skipped entirely.
/// - Content inside a single-backtick span on a line is skipped.
/// - `[[Target]]` → `WikiLink { target: "Target", alias: None, raw: "[[Target]]" }`
/// - `[[Target|Alias]]` → `WikiLink { target: "Target", alias: Some("Alias"), … }`
/// - Duplicate links are preserved (dedup is the caller's responsibility).
/// - An unclosed `[[` on a line is ignored.
fn extract_wiki_links_from_body(body: &str) -> Vec<WikiLink> {
    let mut links = Vec::new();
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim_start();

        // Toggle fenced code block state on lines that start with ``` or ~~~.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        // Scan the line byte-by-byte for wiki-links, skipping inline code spans.
        // All delimiter bytes ([, ], |, `) are ASCII, so byte-level indexing is
        // safe even in UTF-8 strings (multi-byte continuation bytes never equal
        // any ASCII value).
        let s = line.as_bytes();
        let len = s.len();
        let mut pos = 0usize;

        while pos < len {
            // Skip inline code span delimited by single backticks.
            if s[pos] == b'`' {
                pos += 1;
                while pos < len && s[pos] != b'`' {
                    pos += 1;
                }
                if pos < len {
                    pos += 1; // consume closing backtick
                }
                continue;
            }

            // Detect `[[` opening.
            if pos + 1 < len && s[pos] == b'[' && s[pos + 1] == b'[' {
                let link_start = pos;
                pos += 2; // skip [[
                let content_start = pos;

                // Advance until `]]` or end of line.
                let mut closed = false;
                while pos + 1 < len {
                    if s[pos] == b']' && s[pos + 1] == b']' {
                        // Found closing ]].
                        // SAFETY: link_start, content_start, pos, pos+2 are all
                        // on ASCII character boundaries (each is positioned right
                        // after an ASCII delimiter byte).
                        let content = &line[content_start..pos];
                        let raw = &line[link_start..pos + 2];

                        let (target, alias) = match content.split_once('|') {
                            Some((t, a)) => (t.to_string(), Some(a.to_string())),
                            None => (content.to_string(), None),
                        };

                        links.push(WikiLink {
                            target,
                            alias,
                            raw: raw.to_string(),
                        });
                        pos += 2; // skip ]]
                        closed = true;
                        break;
                    }
                    pos += 1;
                }

                if !closed {
                    // Unclosed [[: skip to end of line.
                    break;
                }
                continue;
            }

            pos += 1;
        }
    }

    links
}

// ── Adapter impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl Adapter for ObsidianAdapter {
    fn scheme(&self) -> &'static str {
        "obsidian"
    }

    async fn fetch(&self, uri: &WireUri) -> WireResult<Value> {
        let spec = parse_obsidian_uri(uri.as_raw())?;
        let full_path = spec.vault_root.join(&spec.note_path);

        let raw = tokio::fs::read_to_string(&full_path).await.map_err(|e| {
            WireError::Storage(format!(
                "obsidian adapter: read failed: {}: {e}",
                full_path.display()
            ))
        })?;

        let (frontmatter, body) = if spec.expand_frontmatter {
            parse_frontmatter(&raw)
        } else {
            (Value::Null, raw)
        };

        let mut result = json!({
            "vault_path": spec.vault_root.to_string_lossy(),
            "note_path": spec.note_path.to_string_lossy(),
            "frontmatter": frontmatter,
            "body": body,
        });

        // Add wiki_links only when ?links=edge is specified (default: field omitted).
        if spec.extract_wiki_links {
            let wiki_links = extract_wiki_links_from_body(result["body"].as_str().unwrap_or(""));
            result["wiki_links"] =
                serde_json::to_value(&wiki_links).unwrap_or(Value::Array(vec![]));
        }

        Ok(result)
    }
}

// ── Inline tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Write a note file into a temp directory.
    fn write_note(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    /// Build a `obsidian:///...` URI string for a note in `dir`.
    fn make_uri(dir: &Path, note: &str) -> String {
        format!("obsidian://{}/{}", dir.display(), note)
    }

    /// Build a URI with a query string appended.
    fn make_uri_query(dir: &Path, note: &str, query: &str) -> String {
        format!("obsidian://{}/{}?{}", dir.display(), note, query)
    }

    /// Parse a URI string into a `WireUri`.
    fn wire_uri(s: &str) -> WireUri {
        WireUri::parse(s).expect("valid test uri")
    }

    /// (a) YAML frontmatter is parsed and returned as JSON object.
    #[tokio::test]
    async fn fetch_yaml_frontmatter_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_note(
            dir.path(),
            "test.md",
            "---\ntitle: Hello World\ntags:\n  - rust\n  - obsidian\n---\n# Body\nContent here.\n",
        );
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri(dir.path(), "test.md")))
            .await
            .unwrap();
        assert_eq!(v["frontmatter"]["title"], "Hello World");
        assert!(v["frontmatter"]["tags"].is_array(), "tags should be array");
        let body = v["body"].as_str().unwrap();
        assert!(body.contains("Content here."));
    }

    /// (b) TOML frontmatter is parsed and returned as JSON object.
    #[tokio::test]
    async fn fetch_toml_frontmatter_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_note(
            dir.path(),
            "toml.md",
            "+++\ntitle = \"TOML Note\"\ntags = [\"rust\", \"toml\"]\n+++\n# TOML Body\nHello.\n",
        );
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri(dir.path(), "toml.md")))
            .await
            .unwrap();
        assert_eq!(v["frontmatter"]["title"], "TOML Note");
        assert!(v["frontmatter"]["tags"].is_array(), "tags should be array");
    }

    /// (c) Note without frontmatter — body is the entire file, frontmatter is null.
    #[tokio::test]
    async fn fetch_no_frontmatter_returns_full_body() {
        let dir = tempfile::tempdir().unwrap();
        let content = "# Just a note\nNo frontmatter here.\n";
        write_note(dir.path(), "plain.md", content);
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri(dir.path(), "plain.md")))
            .await
            .unwrap();
        assert!(v["frontmatter"].is_null(), "frontmatter should be null");
        let body = v["body"].as_str().unwrap();
        assert!(body.contains("No frontmatter here."));
    }

    /// (d) Custom key in frontmatter is accessible in the returned JSON.
    #[tokio::test]
    async fn fetch_custom_frontmatter_key() {
        let dir = tempfile::tempdir().unwrap();
        write_note(
            dir.path(),
            "custom.md",
            "---\ncustom_field: my_value\n---\nCustom content.\n",
        );
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri(dir.path(), "custom.md")))
            .await
            .unwrap();
        assert_eq!(v["frontmatter"]["custom_field"], "my_value");
    }

    /// (e) File not found returns `WireError::Storage`.
    #[tokio::test]
    async fn fetch_file_not_found_errors() {
        let dir = tempfile::tempdir().unwrap();
        let r = ObsidianAdapter
            .fetch(&wire_uri(&make_uri(dir.path(), "nonexistent.md")))
            .await;
        assert!(r.is_err(), "expected error for missing file");
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("obsidian adapter"),
            "error should mention adapter: {msg}"
        );
    }

    // ── Wiki-link extraction tests ────────────────────────────────────────────

    /// (f) ?links=edge returns wiki_links with basic [[Note A]] entry.
    #[tokio::test]
    async fn fetch_wiki_links_basic() {
        let dir = tempfile::tempdir().unwrap();
        write_note(
            dir.path(),
            "links.md",
            "# My Note\nSee [[Note A]] and [[Note B]] for more details.\n",
        );
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri_query(
                dir.path(),
                "links.md",
                "links=edge",
            )))
            .await
            .unwrap();

        let wl = v["wiki_links"]
            .as_array()
            .expect("wiki_links should be array");
        assert_eq!(wl.len(), 2, "expected two wiki links");

        // First link: [[Note A]]
        assert_eq!(wl[0]["target"], "Note A");
        assert_eq!(wl[0]["raw"], "[[Note A]]");
        assert!(
            wl[0].get("alias").is_none() || wl[0]["alias"].is_null(),
            "alias should be absent"
        );

        // Second link: [[Note B]]
        assert_eq!(wl[1]["target"], "Note B");
        assert_eq!(wl[1]["raw"], "[[Note B]]");
    }

    /// (g) ?links=edge with [[Target|Alias]] splits target and alias correctly.
    #[tokio::test]
    async fn fetch_wiki_links_alias() {
        let dir = tempfile::tempdir().unwrap();
        write_note(
            dir.path(),
            "alias.md",
            "Check out [[Meeting Notes|Notes]] and [[Project Plan|Plan]].\n",
        );
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri_query(
                dir.path(),
                "alias.md",
                "links=edge",
            )))
            .await
            .unwrap();

        let wl = v["wiki_links"]
            .as_array()
            .expect("wiki_links should be array");
        assert_eq!(wl.len(), 2);

        assert_eq!(wl[0]["target"], "Meeting Notes");
        assert_eq!(wl[0]["alias"], "Notes");
        assert_eq!(wl[0]["raw"], "[[Meeting Notes|Notes]]");

        assert_eq!(wl[1]["target"], "Project Plan");
        assert_eq!(wl[1]["alias"], "Plan");
    }

    /// (h) Without ?links=edge (default off), wiki_links field is absent from result.
    #[tokio::test]
    async fn fetch_wiki_links_off_by_default() {
        let dir = tempfile::tempdir().unwrap();
        write_note(dir.path(), "nolinks.md", "See [[Note A]] here.\n");
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri(dir.path(), "nolinks.md")))
            .await
            .unwrap();

        // wiki_links field must be absent (not null, not empty array — field omit).
        assert!(
            v.get("wiki_links").is_none(),
            "wiki_links should not be present when links=off (default)"
        );
    }

    /// (i) Wiki-links inside fenced code blocks are not extracted.
    #[tokio::test]
    async fn fetch_wiki_links_skip_code_fence() {
        let dir = tempfile::tempdir().unwrap();
        write_note(
            dir.path(),
            "fence.md",
            "Real link: [[Real Note]]\n\
             ```\n\
             This is code: [[Fake Note]]\n\
             ```\n\
             Also real: [[Another Real]]\n",
        );
        let v = ObsidianAdapter
            .fetch(&wire_uri(&make_uri_query(
                dir.path(),
                "fence.md",
                "links=edge",
            )))
            .await
            .unwrap();

        let wl = v["wiki_links"]
            .as_array()
            .expect("wiki_links should be array");
        // Only [[Real Note]] and [[Another Real]] — [[Fake Note]] inside fence is skipped.
        assert_eq!(
            wl.len(),
            2,
            "only links outside fences should appear: {wl:?}"
        );
        assert_eq!(wl[0]["target"], "Real Note");
        assert_eq!(wl[1]["target"], "Another Real");
    }
}
