//! Integration test — runs every default invariant against the
//! full registered catalog. If a future commit registers a new
//! domain that violates an invariant, this test fails with a
//! precise message naming the (invariant, keyword, reason).

use tatara_platform_checks::{default_invariants, run_all, Outcome};

fn register_full_catalog() {
    tatara_gateway_api::register();
    tatara_cilium::register();
    tatara_prometheus_operator::register();
    tatara_ebpf::register();
}

#[test]
fn live_catalog_passes_every_default_invariant() {
    register_full_catalog();
    let invariants = default_invariants();
    let run = run_all(&invariants);
    if run.fail_count() > 0 {
        let report = run.report();
        panic!(
            "Platform invariants failed ({} failure(s)):\n\n{report}",
            run.fail_count()
        );
    }
}

#[test]
fn report_lists_every_invariant_even_when_clean() {
    register_full_catalog();
    let invariants = default_invariants();
    let run = run_all(&invariants);
    let report = run.report();
    for inv in &invariants {
        assert!(
            report.contains(inv.name),
            "report should mention invariant `{}`: \n{report}",
            inv.name
        );
    }
}

#[test]
fn always_required_layers_invariant_catches_missing_handler() {
    // Synthetic test — register only base TataraDomain (no doc /
    // deps / etc) and confirm the invariant fires. We can't
    // easily clear the global registries between tests, so we
    // simulate by running the check with a synthetic keyword
    // that has no registrations at all.
    use std::collections::HashMap;
    let synthetic_keywords: &[&str] = &["defnonexistent"];
    let outcomes = (default_invariants()
        .into_iter()
        .find(|i| i.name == "always-required-layers-present")
        .unwrap()
        .run)(synthetic_keywords);
    let result = &outcomes["defnonexistent"];
    let _: &HashMap<_, _> = &outcomes;
    assert!(
        matches!(result, Outcome::Fail(_)),
        "unregistered keyword should fail the layers invariant: {result:?}"
    );
    if let Outcome::Fail(msg) = result {
        assert!(msg.contains("Doc"));
        assert!(msg.contains("Stability"));
    }
}

#[test]
fn deps_invariant_skips_when_no_deps_registered() {
    // For a synthetic keyword with no deps registration, the
    // outcome is Skip — not Fail. That's the right shape for
    // optional layers.
    let synthetic_keywords: &[&str] = &["defnoexistedp"];
    let outcomes = (default_invariants()
        .into_iter()
        .find(|i| i.name == "deps-resolve-to-registered-keywords")
        .unwrap()
        .run)(synthetic_keywords);
    assert!(matches!(outcomes["defnoexistedp"], Outcome::Skip(_)));
}

#[test]
fn schemas_invariant_passes_for_forge_generated_domains() {
    register_full_catalog();
    let outcomes = (default_invariants()
        .into_iter()
        .find(|i| i.name == "schemas-parse-as-json")
        .unwrap()
        .run)(&tatara_lisp::domain::registered_keywords());
    // Forge-generated keywords have schemas; ebpf keywords don't.
    assert!(matches!(outcomes["defgateway"], Outcome::Pass));
    assert!(matches!(outcomes["defciliumnetworkpolicy"], Outcome::Pass));
    assert!(matches!(outcomes["defbpf-program"], Outcome::Skip(_)));
}
