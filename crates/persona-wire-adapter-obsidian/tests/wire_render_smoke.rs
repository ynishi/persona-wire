//! Integration smoke tests for `persona-wire-adapter-obsidian`.
//!
//! Uses the sample-vault fixtures in `tests/fixtures/sample-vault/` to verify
//! the full fetch pipeline without needing a live Obsidian installation.

use persona_wire_adapter_obsidian::ObsidianAdapter;
use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::infrastructure::adapter::Adapter;
use persona_wire_core::infrastructure::wire_uri::WireUri;

/// Helper: build an absolute `obsidian:///...` URI pointing at a fixture note.
fn fixture_uri(note: &str, query: Option<&str>) -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let vault_path = format!("{}/tests/fixtures/sample-vault", manifest_dir);
    match query {
        Some(q) => format!("obsidian:///{vault_path}/{note}?{q}"),
        None => format!("obsidian:///{vault_path}/{note}"),
    }
}

/// (a) Full feature smoke: YAML frontmatter + wiki-links (`?links=edge`).
///
/// Verifies:
/// - `frontmatter.title` parses correctly.
/// - `[[Other Note]]` and `[[Linked Note|表示テキスト]]` are extracted.
/// - `[[fake link ...]]` inside a fenced code block is NOT extracted.
#[tokio::test]
async fn fetch_yaml_note_with_links_edge() {
    let uri_str = fixture_uri("note-with-yaml-frontmatter.md", Some("links=edge"));
    let uri = WireUri::parse(&uri_str).unwrap();
    let adapter = ObsidianAdapter;

    let result = adapter.fetch(&uri).await.unwrap();

    // Frontmatter is present and title matches.
    assert!(
        result["frontmatter"].is_object(),
        "frontmatter should be an object"
    );
    assert_eq!(
        result["frontmatter"]["title"].as_str(),
        Some("Note with YAML Frontmatter"),
        "title should match fixture"
    );

    // wiki_links array is present.
    let links = result["wiki_links"]
        .as_array()
        .expect("wiki_links should be an array when ?links=edge");

    // Basic link [[Other Note]] is extracted.
    assert!(
        links
            .iter()
            .any(|l| l["target"].as_str() == Some("Other Note")),
        "[[Other Note]] should appear in wiki_links"
    );

    // Alias link [[Linked Note|表示テキスト]] splits target and alias correctly.
    assert!(
        links.iter().any(|l| {
            l["target"].as_str() == Some("Linked Note")
                && l["alias"].as_str() == Some("表示テキスト")
        }),
        "[[Linked Note|表示テキスト]] should appear with correct target and alias"
    );

    // [[fake link ...]] inside fenced code block must NOT be extracted.
    let fake_target = "fake link inside code fence — should NOT be extracted";
    assert!(
        !links
            .iter()
            .any(|l| l["target"].as_str() == Some(fake_target)),
        "wiki-link inside fenced code block must be skipped"
    );
}

/// (b) Default (links off): `wiki_links` field must be absent.
///
/// Verifies that a plain note fetched without `?links=edge` does not include
/// the `wiki_links` field in the returned JSON.
#[tokio::test]
async fn fetch_plain_note_default_no_wiki_links() {
    let uri_str = fixture_uri("note-plain.md", Some("frontmatter=on"));
    let uri = WireUri::parse(&uri_str).unwrap();
    let adapter = ObsidianAdapter;

    let result = adapter.fetch(&uri).await.unwrap();

    // `wiki_links` must be absent (not null, not empty array) when links=off.
    assert!(
        result.get("wiki_links").is_none(),
        "wiki_links must not appear when ?links=edge is not set"
    );

    // Body is a non-empty string.
    assert!(
        result["body"].as_str().is_some_and(|b| !b.is_empty()),
        "body should be a non-empty string"
    );
}

/// (c) PluginRegistry chain: `ObsidianAdapter` integrates via registry dispatch.
///
/// Verifies that `ObsidianAdapter` can be registered in a `PluginRegistry` and
/// dispatched correctly via `adapter_for_uri`.
#[tokio::test]
async fn fetch_via_plugin_registry() {
    let registry = PluginRegistry::default_builder_for_wire()
        .with_adapter(ObsidianAdapter)
        .build()
        .unwrap();

    let uri_str = fixture_uri("note-plain.md", None);
    let uri = WireUri::parse(&uri_str).unwrap();

    let adapter = registry
        .adapter_for_uri(uri.as_raw())
        .expect("obsidian adapter should be registered");

    let result = adapter.fetch(&uri).await.unwrap();

    assert!(
        result["body"].is_string(),
        "body should be a string from registry dispatch"
    );
    assert_eq!(
        result["body"].as_str().map(|b| b.contains("plain")),
        Some(true),
        "body should contain text from note-plain.md"
    );
}
