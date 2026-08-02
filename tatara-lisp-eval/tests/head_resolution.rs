//! `Interpreter::resolve_head` — all three arbiters, in the evaluator's order.
//!
//! A consumer asking "is this name already taken" almost always reaches for an
//! environment lookup, which sees exactly one of the three. The two it misses
//! are the two that have actually bitten: a macro shadowed blue's `assert` and
//! made every assertion in its suite silently pass, and special forms are
//! `match` arms that appear in no environment at all.
//!
//! Each test below names the arbiter it covers, so a gate built on this cannot
//! quietly cover only the easy one.

use std::collections::BTreeSet;

use tatara_lisp_eval::{install_full_stdlib_with, HeadBinding, Interpreter};

fn composed() -> Interpreter<()> {
    let mut i = Interpreter::new();
    let mut host = ();
    install_full_stdlib_with(&mut i, &mut host);
    i
}

/// Arbiter 1: a special form. Invisible to the environment — this is the one
/// an environment-only gate misses completely.
#[test]
fn a_special_form_resolves_as_a_special_form_and_is_absent_from_the_environment() {
    let i = composed();
    for name in ["if", "not", "and", "or", "when", "let", "quote", "define"] {
        assert_eq!(
            i.resolve_head(name),
            Some(HeadBinding::SpecialForm),
            "`{name}` is a special form"
        );
        assert!(
            i.globals_snapshot().lookup(name).is_none(),
            "`{name}` must be absent from the environment — that absence is \
             exactly why an environment-only check reports it as free"
        );
    }
}

/// Arbiter 2: a macro. Beats a same-named binding because it rewrites the form
/// before evaluation, which is what made blue's shadowed `assert` silent.
#[test]
fn a_macro_resolves_as_a_macro() {
    let i = composed();
    let macros: Vec<&str> = ["assert"]
        .into_iter()
        .filter(|n| i.resolve_head(n) == Some(HeadBinding::Macro))
        .collect();
    assert!(
        !macros.is_empty(),
        "the composed stdlib must register at least one macro for this test to \
         mean anything; if `assert` moved, point this at whatever replaced it"
    );
}

/// Arbiter 3: an ordinary binding.
#[test]
fn a_binding_resolves_as_a_value() {
    let i = composed();
    for name in ["+", "map", "foldl"] {
        assert_eq!(
            i.resolve_head(name),
            Some(HeadBinding::Value),
            "`{name}` is an ordinary binding"
        );
    }
}

/// Anti-vacuity. A `resolve_head` that answered `Some(_)` for everything would
/// pass every test above.
#[test]
fn a_name_nothing_claims_resolves_to_nothing() {
    let i = composed();
    for name in [
        "blue-assert",
        "definitely-not-a-tatara-name",
        "hash-map-xyzzy",
    ] {
        assert_eq!(i.resolve_head(name), None, "`{name}` must be free");
    }
}

/// The three arbiters are distinguished, not merged. Without this a
/// `resolve_head` returning `Value` for all three would satisfy the shape of
/// every test above that does not pin the variant.
#[test]
fn the_three_arbiters_are_told_apart() {
    let i = composed();
    let kinds: BTreeSet<_> = ["if", "assert", "+"]
        .iter()
        .filter_map(|n| i.resolve_head(n))
        .collect();
    assert_eq!(
        kinds.len(),
        3,
        "expected one of each arbiter, got {kinds:?} — a resolver that cannot \
         distinguish them cannot explain WHY a name is taken, which is the \
         difference between a usable diagnostic and 'already bound'"
    );
}

/// `reserved_head_names` is the union, and it must include names from each
/// arbiter — in particular the special forms, which no environment holds.
#[test]
fn the_reserved_set_spans_every_arbiter() {
    let i = composed();
    let reserved = i.reserved_head_names();
    let has = |n: &str| reserved.iter().any(|r| &**r == n);

    assert!(has("if"), "a special form must be in the reserved set");
    assert!(has("assert"), "a macro must be in the reserved set");
    assert!(has("+"), "a binding must be in the reserved set");
    assert!(!has("blue-assert"), "and a free name must not be");
}

/// The union agrees with the per-name resolver. Two answers to one question is
/// how they drift; this makes drift a red build rather than a surprise.
#[test]
fn the_reserved_set_agrees_with_the_resolver() {
    let i = composed();
    for name in i.reserved_head_names() {
        assert!(
            i.resolve_head(&name).is_some(),
            "`{name}` is in the reserved set but resolves to nothing"
        );
    }
}
