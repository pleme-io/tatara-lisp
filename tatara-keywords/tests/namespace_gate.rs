//! The gate itself, run against this repo's own tree and its own ledger.
//!
//! `cargo test` is the enforcement surface here, deliberately: this repo is
//! trunk-based and `pre-merge-gate.yml` has never executed (`on: pull_request`
//! only), so a check that lives in a workflow is a declaration, not a gate.
//! These run every time anybody types `cargo test`.
//!
//! **Tier-honest: CI-caught, not compile-caught.** Nothing here makes a
//! duplicate keyword unrepresentable. A second `#[tatara(keyword = "defenv")]`
//! still compiles; what changes is that it can no longer land green.

use std::path::{Path, PathBuf};

use tatara_keywords::{collisions, scan, trespasses, Declaration, Reservas};

/// The repo root, from this crate's manifest dir.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tatara-keywords lives one level under the repo root")
        .to_path_buf()
}

fn ledger() -> Reservas {
    let path = repo_root().join("keywords.tlisp");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the ledger must exist at {}: {e}", path.display()));
    Reservas::from_lisp(&src).expect("the committed ledger must parse as tatara-lisp")
}

fn our_declarations() -> Vec<Declaration> {
    scan(&repo_root()).expect("scanning this repo must succeed")
}

/// Calibration. Every assertion below is about a set of declarations; a scan
/// that returned none would satisfy all of them vacuously, and the resulting
/// green would be the exact defect this crate exists to end.
#[test]
fn the_scan_finds_this_repo_s_own_declarations() {
    let decls = our_declarations();
    assert!(
        decls.len() >= 10,
        "expected this repo to declare at least ten keywords; found {} — \
         a low count here means the scan is broken, not that the repo shrank",
        decls.len()
    );
    assert!(
        decls.iter().any(|d| d.keyword == "defreservas"),
        "the ledger's own keyword must be among the scanned declarations"
    );
}

#[test]
fn no_two_types_in_this_repo_claim_one_keyword() {
    let found = collisions(&our_declarations());
    assert!(
        found.is_empty(),
        "keyword collision(s) inside this repo:\n{}",
        found
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .concat()
    );
}

#[test]
fn this_repo_claims_no_keyword_the_ledger_reserves_elsewhere() {
    let found = trespasses(&our_declarations(), &ledger(), "tatara-lisp");
    assert!(
        found.is_empty(),
        "this repo declares keyword(s) reserved to another repo:\n{}",
        found
            .iter()
            .map(|t| format!("  {t}\n"))
            .collect::<Vec<_>>()
            .concat()
    );
}

/// Models-stay-current, mechanically. A keyword added here without re-running
/// `tatara-keywords census --emit-ledger` leaves the committed ledger claiming
/// a namespace it no longer describes — and every OTHER repo checks against
/// that stale copy, so the omission silently frees a taken keyword fleet-wide.
#[test]
fn every_keyword_this_repo_declares_is_recorded_in_the_ledger() {
    let ledger = ledger();
    let index = ledger.index();
    let missing: Vec<&Declaration> = our_declarations()
        .iter()
        .filter(|d| !index.contains_key(d.keyword.as_str()))
        .map(|d| Box::leak(Box::new(d.clone())) as &Declaration)
        .collect();
    assert!(
        missing.is_empty(),
        "keyword(s) declared here but absent from keywords.tlisp — re-run\n  \
         cargo run -p tatara-keywords -- census --tree <org-root> --emit-ledger \\\n    \
         --measured-on <today> --arvore '~/code/github/pleme-io' > keywords.tlisp\n{}",
        missing
            .iter()
            .map(|d| format!("  {} at {}:{}\n", d.keyword, d.path.display(), d.line))
            .collect::<Vec<_>>()
            .concat()
    );
}

/// The gate's own red run, kept as a test rather than as a memory of one.
///
/// A gate is only worth what its failing case proves, and a failing case that
/// exists once in a terminal is not evidence anybody can re-check. This builds
/// the violating input in a temp tree and asserts the checker sees it.
#[test]
fn a_freshly_added_duplicate_is_caught() {
    let dir = std::env::temp_dir().join(format!(
        "tatara-keywords-gate-{}-{}",
        std::process::id(),
        line!()
    ));
    let a = dir.join("crate-a/src");
    let b = dir.join("crate-b/src");
    std::fs::create_dir_all(&a).expect("temp tree");
    std::fs::create_dir_all(&b).expect("temp tree");
    std::fs::write(
        a.join("lib.rs"),
        "#[derive(TataraDomain)]\n#[tatara(keyword = \"defduplicata\")]\npub struct A;\n",
    )
    .expect("write");
    std::fs::write(
        b.join("lib.rs"),
        "#[derive(TataraDomain)]\n#[tatara(keyword = \"defduplicata\")]\npub struct B;\n",
    )
    .expect("write");

    let decls = scan(&dir).expect("scan temp tree");
    let found = collisions(&decls);

    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(found.len(), 1, "the duplicate must be caught: {found:?}");
    assert_eq!(found[0].keyword, "defduplicata");
    assert_eq!(found[0].declarations.len(), 2);
}
