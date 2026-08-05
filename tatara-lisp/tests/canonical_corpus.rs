//! Formatted-ness as a build-gated property of THIS repo's lisp corpus.
//!
//! `gofmt` and `rustfmt` are conventions — a CI job, a hook, a habit — and all
//! three are bypassable. `caixa-fmt::is_canonical` makes formatted-ness a fact
//! about the LANGUAGE instead: it parses, re-renders, and compares bytes.
//! This test points that gate at every `.tlisp` / `.lisp` file in the repo, so
//! `cargo test` fails on a non-canonical source rather than a linter mentioning
//! it later.
//!
//! ## Why this is a dev-dependency, and why that is not a cycle problem
//!
//! `caixa-fmt` depends on `tatara-lisp`. A normal dependency the other way
//! would be a hard cargo cycle — but cargo permits DEV-dependency cycles
//! precisely because test code is a separate compilation unit. Verified before
//! relying on it: `cargo metadata` exits 0 with this edge in place.
//!
//! That the canonical formatter for tatara-lisp lives ABOVE tatara-lisp is
//! itself the architectural oddity here. The reason is real rather than
//! historical: tatara-lisp's reader discards spans and trivia, so it cannot
//! re-render source without deleting every comment — which is exactly why
//! `caixa-ast` exists as a sibling AST that keeps them. Until tatara-lisp's own
//! reader carries trivia, the canonical form cannot move down, and this test is
//! the closest the gate can sit to the compiler.
//!
//! ## The `feira` CLI and this library DISAGREE — a real defect, named here
//!
//! Both call themselves 0.1.5, and they format the same input differently.
//! Measured on `keywords.tlisp` line 14: the library explodes a `:palavras`
//! entry across three lines, the CLI keeps it on one. Two of the four corpus
//! files differ under it.
//!
//! They are not the same build. The library is crates.io `caixa-fmt` 0.1.5,
//! which is immutable. The CLI resolves to a nix-store `rust_caixa-feira-0.1.5`
//! built from a checkout that need not match the published crate — so the
//! version string is a coincidence of numbering, not evidence of agreement.
//!
//! **This gate treats the LIBRARY as authoritative**, because it is the thing
//! actually linked here and the only one of the two that is reproducible from a
//! pin. That makes `feira fmt` actively harmful against this corpus: it
//! reformats files into a state this test rejects. Hence the in-repo
//! `reformat` test below rather than a documented CLI invocation.
//!
//! **This is worth fixing upstream in caixa-fmt, and is not fixed here.** One
//! canonical form is the entire premise of `is_canonical`; two renderings
//! wearing one version number means "canonical" currently depends on which
//! artifact you asked. Cutting the CLI over to the published library — or
//! publishing the CLI's — is the actual repair, and it belongs in caixa-fmt.
//!
//! ## Tier — read before citing this as a guarantee
//!
//! **Test-caught, not parse-rejected.** A non-canonical file in this repo fails
//! `cargo test`; it does not fail `cargo build`, and nothing stops another
//! crate from calling `tatara_lisp::read` on badly-formatted text. The honest
//! statement of the ceiling is in caixa-fmt's own module docs: the
//! truly-parse-rejected tier arrives when the EVALUATOR calls `parse_canonical`,
//! which is a separate flag day with its own ordering constraint.
//!
//! What this does buy is that the corpus cannot silently drift back out of
//! canonical form, which is what "step 2 then step 3" of that ordering needs in
//! order to hold.

use std::path::{Path, PathBuf};

/// Every `.tlisp` / `.lisp` file in the repository, excluding build output.
///
/// Walked rather than hard-listed on purpose: a hard list silently stops
/// covering a file added later, and a gate whose denominator can quietly shrink
/// is the failure mode this fleet keeps rediscovering.
fn corpus(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // `target` is build output and `.git` is not source.
            if name != "target" && name != ".git" {
                corpus(&path, out);
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("tlisp") | Some("lisp")
        ) {
            out.push(path);
        }
    }
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/tatara-lisp`; the corpus spans the whole
    // workspace, so climb one level.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

#[test]
fn every_lisp_source_is_canonically_formatted() {
    let root = repo_root();
    let mut files = Vec::new();
    corpus(&root, &mut files);
    files.sort();

    // A zero denominator would make this pass by finding nothing, which reads
    // identically to "everything is fine". The repo has four such files today;
    // the floor sits below that so adding or moving one does not wedge the
    // gate, while a walk that collapses still goes red.
    assert!(
        files.len() >= 3,
        "corpus walk found {} lisp file(s) under {} — expected at least 3. \
         A gate that scans nothing reports success having checked nothing.",
        files.len(),
        root.display()
    );

    let mut offenders = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).expect("readable lisp source");
        if !caixa_fmt::is_canonical(&src) {
            offenders.push(path.strip_prefix(&root).unwrap_or(path).display().to_string());
        }
    }

    assert!(
        offenders.is_empty(),
        "{} of {} lisp source(s) are not canonically formatted:\n  {}\n\n\
         Fix with: cargo test -p tatara-lisp --test canonical_corpus -- \
         --ignored reformat\n\n\
         Do NOT reach for `feira fmt` — the CLI on PATH disagrees with the \
         pinned library and would reformat these files straight back into a \
         red gate. See the CLI-divergence note in this file's module docs.",
        offenders.len(),
        files.len(),
        offenders.join("\n  ")
    );
}

/// The reformatter, kept in-repo so fixing a red run needs no external binary.
///
/// Run with `cargo test -p tatara-lisp --test canonical_corpus -- --ignored
/// reformat`. It writes through the SAME pinned library this gate reads, which
/// is the whole point — see the CLI-divergence note in the module docs above.
#[test]
#[ignore = "mutates the corpus; run deliberately to fix a red gate"]
fn reformat() {
    let root = repo_root();
    let mut files = Vec::new();
    corpus(&root, &mut files);
    files.sort();

    let cfg = caixa_fmt::FmtConfig::default();
    for path in &files {
        let src = std::fs::read_to_string(path).expect("readable lisp source");
        match caixa_fmt::format_source(&src, &cfg) {
            Ok(formatted) if formatted != src => {
                std::fs::write(path, &formatted).expect("writable lisp source");
                println!("reformatted {}", path.display());
            }
            Ok(_) => {}
            // A parse failure is reported, never silently skipped: a file the
            // formatter cannot read is exactly the one worth knowing about.
            Err(e) => println!("PARSE FAILED {}: {e}", path.display()),
        }
    }
}
