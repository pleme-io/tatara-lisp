//! `bound_symbol_names` — the single set both `tatara-script symbols` and the
//! lint's `unbound-symbol` rule read.
//!
//! The property that matters is AGREEMENT: a name `symbols` lists must be a
//! name a script can actually call, and a name it omits must genuinely fail.
//! These tests check the set against the live evaluator rather than against a
//! remembered list, which would drift exactly the way the lint's own comment
//! warns about.

use tatara_lisp_script::{bound_symbol_names, eval_str};

#[test]
fn the_set_is_non_trivial() {
    // A vacuity guard: an empty or near-empty set would make every check below
    // pass trivially and would make the lint reject every script.
    let n = bound_symbol_names().len();
    assert!(n > 200, "expected hundreds of bound names, got {n}");
}

#[test]
fn names_the_fleet_kept_re_deriving_are_listed() {
    // Each of these was hand-rolled or probed-for by trial and error across the
    // fleet while being bound. Listing them is the point of `symbols`.
    let names = bound_symbol_names();
    for n in ["member?", "first", "alist-get", "exec-capture",
              "status-of", "stdout-of", "stderr-of", "string-lowercase"] {
        assert!(names.contains(n), "{n} is bound but missing from the set");
    }
}

#[test]
fn a_name_that_is_not_bound_is_absent() {
    // The negative control. `string-downcase` is the misspelling the lint's own
    // diagnostic cites; it must not appear, or `symbols` would send an author
    // to a name that fails at runtime.
    let names = bound_symbol_names();
    assert!(!names.contains("string-downcase"));
    assert!(eval_str("(string-downcase \"X\")").is_err(),
            "negative control is only meaningful if the name really is unbound");
}

#[test]
fn every_listed_primitive_is_actually_resolvable() {
    // Agreement in the other direction, on a sample that exercises the three
    // install layers: a native from process.rs, a Lisp-stdlib definition, and
    // a json.rs native. Referencing the bare symbol must not raise unbound.
    for n in ["status-of", "member?", "alist-get"] {
        assert!(bound_symbol_names().contains(n));
        assert!(eval_str(n).is_ok(), "{n} is listed but does not resolve");
    }
}

#[test]
fn the_set_is_stable_across_calls() {
    // A fresh interpreter each call; the result must not depend on call order.
    assert_eq!(bound_symbol_names(), bound_symbol_names());
}
