//! Tests for `default_db_path()` fallback resolution.
//!
//! Covers the persona-x family convention (persona-work pattern):
//! - `$XDG_DATA_HOME/persona-wire/store.db` if `XDG_DATA_HOME` is set
//! - `$HOME/.persona-wire/store.db` otherwise
//!
//! Env override (`PERSONA_WIRE_DB`) and CLI `--db` flag precedence are
//! handled at the CLI/MCP wrapper layer (`crates/persona-wire/src/main.rs`),
//! so they're not part of this core-level helper test.
//!
//! NB: env mutation is process-wide and shared across threads. All tests in
//! this file run sequentially (`cargo test --workspace` is parallel by
//! default, but each test in *this* file mutates `XDG_DATA_HOME` / `HOME`
//! exclusively — they each save & restore around their body, and Rust's
//! `#[test]` infrastructure already serialises within a single file unless
//! `--test-threads` is increased; we additionally guard with a Mutex below).

use std::sync::Mutex;

use persona_wire_core::infrastructure::storage::default_db_path;

// Serialise env-mutating tests within this file.
static ENV_GUARD: Mutex<()> = Mutex::new(());

/// Snapshot + RAII restore for environment variables touched by the tests,
/// so a panic in the middle of a test doesn't leak state into siblings.
struct EnvSnapshot {
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
}

impl EnvSnapshot {
    fn capture() -> Self {
        Self {
            xdg: std::env::var_os("XDG_DATA_HOME"),
            home: std::env::var_os("HOME"),
        }
    }
}

impl Drop for EnvSnapshot {
    fn drop(&mut self) {
        match &self.xdg {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match &self.home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn fallback_uses_xdg_data_home_when_set() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _snap = EnvSnapshot::capture();

    std::env::set_var("XDG_DATA_HOME", "/tmp/test-xdg-data");
    // HOME doesn't matter when XDG_DATA_HOME is set; clear it to be sure.
    std::env::remove_var("HOME");

    let p = default_db_path().expect("resolve");
    assert_eq!(
        p.to_str().unwrap(),
        "/tmp/test-xdg-data/persona-wire/store.db"
    );
}

#[test]
fn fallback_uses_home_dotfile_when_xdg_unset() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _snap = EnvSnapshot::capture();

    std::env::remove_var("XDG_DATA_HOME");
    std::env::set_var("HOME", "/tmp/test-home");

    let p = default_db_path().expect("resolve");
    assert_eq!(p.to_str().unwrap(), "/tmp/test-home/.persona-wire/store.db");
}

#[test]
fn fallback_errors_when_neither_xdg_nor_home_is_set() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _snap = EnvSnapshot::capture();

    std::env::remove_var("XDG_DATA_HOME");
    std::env::remove_var("HOME");

    let err = default_db_path().expect_err("expected error when HOME unset");
    let msg = err.to_string();
    assert!(
        msg.contains("HOME"),
        "expected error message to mention HOME, got: {msg}"
    );
}
