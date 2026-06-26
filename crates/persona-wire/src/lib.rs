//! persona-wire shared library — currently houses only the schema migration
//! framework consumed by the `pw-migrate` binary (and the deprecated
//! `migrate_id_to_ulid` alias). Other crate code lives in `main.rs` /
//! `bin/`; only modules that need to be shared across binaries (or used
//! by external integration tests) go here.

pub mod migrations;
