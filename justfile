# Convenience wrappers around common workflows.
# List recipes: `just` / `just --list`.

# Common Rust workflow shortcuts.
test:
    cargo test --workspace

check:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --check
    cargo check --workspace

# Local install (all bins in the persona-wire crate).
install:
    cargo install --path crates/persona-wire

# Dry-run cargo-dist plan (verifies dist-workspace.toml locally).
dist-plan:
    dist plan

# Regenerate .github/workflows/release.yml from dist-workspace.toml.
# `allow-dirty = ["ci"]` in dist-workspace.toml keeps the hand-maintained
# jobs (currently none, but reserved for future Docker / MCP Registry
# additions) from being clobbered.
dist-generate:
    dist generate --mode=ci

# Generate LLM-facing doc artifacts (llms.txt / per-crate narrative md /
# API JSON) into docs/aidoc/ via cargo-aidoc.
# Prerequisites: `rustup toolchain install nightly` +
# `cargo install cargo-aidoc` (or use local dev build from ../cargo-aidoc).
aidoc:
    cargo aidoc

# CI-friendly drift check: exit 2 if generated artifacts differ from disk.
aidoc-check:
    cargo aidoc --check --strict
