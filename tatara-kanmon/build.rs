//! 関門 — the barrier gate. A non-canonically-formatted lisp source in this
//! workspace does not COMPILE.
//!
//! ## Why this is a whole crate instead of a `build.rs` on `tatara-lisp`
//!
//! Both spellings make `cargo build` fail, and only one of them is free.
//!
//! A `build.rs` on `tatara-lisp` itself would put `caixa-fmt` — plus the second
//! `tatara-lisp` that `caixa-fmt` depends on — into the build graph of EVERY
//! downstream consumer of this crate. Cargo builds build-dependencies
//! unconditionally, so that cost is paid by everyone who merely wants the
//! runtime, forever, to enforce a property of THIS repo's own sources. Putting
//! a formatter in the build graph of a language runtime is a regression for
//! every consumer.
//!
//! This workspace is VIRTUAL (the root `Cargo.toml` has no `[package]`), so a
//! bare `cargo build` builds every member — including this one. Nothing depends
//! on `tatara-kanmon` and it is `publish = false`, so the entire cost lands
//! here and reaches no consumer.
//!
//! ## Tier — what this does and does not buy
//!
//! **Build-rejected at the workspace boundary.** `cargo build`, `cargo check`
//! and `cargo test` all fail on a non-canonical corpus file, because all three
//! build this member. That is genuinely compile-time, not a test assertion.
//!
//! It is still NOT `parse-time-rejected`: `tatara_lisp::read` will happily
//! accept badly-formatted text at runtime, and `cargo build -p tatara-lisp`
//! alone skips this member. Sealing THAT requires the canonical renderer to
//! live at or below `tatara-lisp` so the reader itself can refuse — blocked
//! today because `caixa-fmt` AND `caixa-ast` both take a normal dependency on
//! `tatara-lisp`, making any edge back a hard cargo cycle, and because the
//! reader discards trivia (`reader.rs` emits no token for it). Do not round
//! this up to "the reader rejects it".
//!
//! The dev-dependency in `tatara-lisp/tests/canonical_corpus.rs` stays: it
//! gives a better message and carries the `--ignored reformat` fixer. This is
//! the gate; that is the ergonomics.

use std::path::{Path, PathBuf};

/// Every `.tlisp` / `.lisp` file in the workspace, excluding build output.
///
/// Walked rather than hard-listed: a hard list silently stops covering a file
/// added later, and a gate whose denominator can quietly shrink is the exact
/// false-green this fleet keeps rediscovering.
fn corpus(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    // Re-run when a file appears or disappears in this directory: cargo tracks
    // directory mtimes, and without this a NEWLY ADDED bad file would not
    // re-trigger the gate until something else invalidated it.
    println!("cargo:rerun-if-changed={}", root.display());

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "target" && name != ".git" {
                corpus(&path, out);
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("tlisp") | Some("lisp")
        ) {
            println!("cargo:rerun-if-changed={}", path.display());
            out.push(path);
        }
    }
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf();

    let mut files = Vec::new();
    corpus(&root, &mut files);
    files.sort();

    // A zero denominator passes by finding nothing, which reads identically to
    // "everything is fine". The floor sits below today's count so moving a file
    // does not wedge the build, while a collapsed walk still goes red.
    assert!(
        files.len() >= 3,
        "tatara-kanmon: corpus walk found {} lisp file(s) under {} — expected \
         at least 3. A gate that scans nothing reports success having checked \
         nothing.",
        files.len(),
        root.display()
    );

    let cfg = caixa_fmt::FmtConfig::default();
    let mut offenders = Vec::new();
    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        // `is_canonical` parses, re-renders and compares bytes, so this is the
        // formatter's own verdict rather than a reimplementation of it.
        if !caixa_fmt::is_canonical(&src) {
            let shown = path.strip_prefix(&root).unwrap_or(path);
            // A file the formatter cannot even parse is reported as such rather
            // than lumped in with a layout difference — they need different fixes.
            let why = match caixa_fmt::format_source(&src, &cfg) {
                Ok(_) => "not canonically formatted",
                Err(_) => "DOES NOT PARSE",
            };
            offenders.push(format!("{} ({why})", shown.display()));
        }
    }

    if !offenders.is_empty() {
        panic!(
            "\n\n  {} of {} lisp source(s) blocked the build:\n    {}\n\n  \
             Fix with:\n    cargo test -p tatara-kanmon --test canonical_corpus \
             -- --ignored reformat\n\n  \
             Do NOT reach for `feira fmt`: the CLI on PATH disagrees with the \
             pinned library\n  and would reformat these straight back into a \
             red build.\n\n",
            offenders.len(),
            files.len(),
            offenders.join("\n    ")
        );
    }
}
