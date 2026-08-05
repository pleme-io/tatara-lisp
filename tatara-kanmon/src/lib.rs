//! 関門 — the barrier gate.
//!
//! This crate is deliberately EMPTY. All of its work happens in `build.rs`,
//! which refuses to build when any lisp source in this workspace is not
//! canonically formatted. A package needs a target to exist, so this file is
//! that target and nothing more.
//!
//! Nothing should ever depend on this crate. It earns its place by being a
//! workspace member — a bare `cargo build` builds every member of a virtual
//! workspace, which is what makes the formatting check a COMPILE-time property
//! rather than a test assertion. See `build.rs` for the full rationale and the
//! honest statement of what tier this reaches.
