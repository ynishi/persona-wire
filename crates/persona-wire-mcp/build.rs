//! Build-time sync guard for the bundled onboarding doc.
//!
//! The crate ships a synced copy of the canonical workspace doc
//! `docs/onboarding.md` at `crates/persona-wire-mcp/onboarding.md` so that
//! `cargo publish` (which packages only files inside the crate's own tree)
//! can include it via `include_str!("../onboarding.md")`.
//!
//! This build script protects the in-sync invariant:
//!
//! - **Dev build (workspace `docs/onboarding.md` exists)** — byte-compare the
//!   two copies and `panic!` with a one-line fix command if they diverge.
//! - **Published-tarball build (workspace doc absent)** — skip (= consumer's
//!   `cargo build` from crates.io has no workspace doc to compare against;
//!   only the in-crate copy ships in the tarball, so safety net #1 = file
//!   existence is sufficient there).

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crate_copy = manifest_dir.join("onboarding.md");
    let workspace_copy = manifest_dir.join("../../docs/onboarding.md");

    println!("cargo:rerun-if-changed=onboarding.md");
    println!("cargo:rerun-if-changed=../../docs/onboarding.md");

    if !workspace_copy.exists() {
        // Published tarball build — only the in-crate copy ships, nothing
        // to compare against. Safety net #1 (= include_str! fails if
        // onboarding.md missing) is sufficient here.
        return;
    }

    let crate_bytes =
        fs::read(&crate_copy).unwrap_or_else(|e| panic!("read {:?}: {e}", crate_copy));
    let workspace_bytes =
        fs::read(&workspace_copy).unwrap_or_else(|e| panic!("read {:?}: {e}", workspace_copy));

    if crate_bytes != workspace_bytes {
        panic!(
            "onboarding.md sync drift detected.\n  \
             canonical: {workspace}\n  \
             bundled:   {bundled}\n  \
             fix: cp {workspace} {bundled}",
            workspace = workspace_copy.display(),
            bundled = crate_copy.display()
        );
    }
}
