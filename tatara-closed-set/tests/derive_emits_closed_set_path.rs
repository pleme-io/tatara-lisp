//! The gate on phase 2 step 1's ONE load-bearing edit: `#[derive(ClosedSet)]`
//! must emit `::tatara_closed_set::ClosedSet`, not `::tatara_lisp::ClosedSet`.
//!
//! Why a test and not a grep: the emitted path is only exercised when the
//! derive output is actually compiled against a crate graph where
//! `tatara_lisp::ClosedSet` does NOT exist. That is exactly this crate's
//! graph — `tatara-closed-set` depends on `tatara-lisp`, and `tatara-lisp`
//! deliberately neither carries nor re-exports `ClosedSet`. So if the derive
//! still emitted the old path, THIS FILE would not compile. The gate is
//! compile-time, which is the honest tier — an absent path, not a runtime
//! check.
//!
//! FAIL-ONCE RECORD, measured 2026-07-29 (see the commit message): flipping
//! the derive's emitted trait path back to `::tatara_lisp::ClosedSet` and
//! re-running `cargo test -p tatara-closed-set --test
//! derive_emits_closed_set_path` produced, verbatim:
//!
//! ```text
//! error[E0405]: cannot find trait `ClosedSet` in crate `tatara_lisp`
//!   --> tatara-closed-set/tests/derive_emits_closed_set_path.rs:24:45
//! error[E0405]: cannot find trait `ClosedSet` in crate `tatara_lisp`
//!   --> tatara-closed-set/tests/derive_emits_closed_set_path.rs:47:45
//! error: could not compile `tatara-closed-set` (test
//!        "derive_emits_closed_set_path") due to 2 previous errors
//! ```
//!
//! It has been seen to fail.

use tatara_closed_set::{assert_closed_set_well_formed, ClosedSet, DeriveClosedSet};

/// Exercises every axis the derive can emit at once: the auto-derived
/// parse-rejection carrier (`generate_unknown`), the `Display` companion,
/// the default `FromStr` delegation, and a non-default projection name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, DeriveClosedSet)]
#[closed_set(via = "as_str", generate_unknown, display)]
pub enum ChannelKind {
    HttpEvent,
    NatsSubject,
    Stdout,
}

impl ChannelKind {
    pub const ALL: [Self; 3] = [Self::HttpEvent, Self::NatsSubject, Self::Stdout];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpEvent => "HttpEvent",
            Self::NatsSubject => "NatsSubject",
            Self::Stdout => "Stdout",
        }
    }
}

/// A second implementor on the OTHER axis of the derive: an operator-pinned
/// irregular label, and `no_from_str` for an enum that keys on something
/// other than a bare label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, DeriveClosedSet)]
#[closed_set(generate_unknown = "macro definition head", no_from_str)]
pub enum MacroDefHead {
    Defmacro,
    DefmacroStar,
}

impl MacroDefHead {
    pub const ALL: [Self; 2] = [Self::Defmacro, Self::DefmacroStar];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Defmacro => "defmacro",
            Self::DefmacroStar => "defmacro*",
        }
    }
}

#[test]
fn derive_emits_a_trait_impl_reachable_as_tatara_closed_set_closed_set() {
    // Fully-qualified through the crate path the derive emits. If the
    // derive named a different crate this line would not resolve.
    assert_eq!(
        <ChannelKind as tatara_closed_set::ClosedSet>::ALL,
        &ChannelKind::ALL
    );
    assert_eq!(<ChannelKind as ClosedSet>::CARDINALITY, 3);
}

#[test]
fn derive_threads_the_via_projection_into_label() {
    assert_eq!(ChannelKind::NatsSubject.label(), "NatsSubject");
}

#[test]
fn derive_emits_the_from_str_delegation_by_default() {
    assert_eq!("Stdout".parse::<ChannelKind>(), Ok(ChannelKind::Stdout));
    let rejected = "stdout".parse::<ChannelKind>().unwrap_err();
    assert_eq!(rejected.to_string(), "unknown channel kind: stdout");
}

#[test]
fn derive_emits_the_display_companion_when_flagged() {
    assert_eq!(ChannelKind::HttpEvent.to_string(), "HttpEvent");
}

#[test]
fn derive_honours_the_operator_pinned_set_label() {
    let rejected = <MacroDefHead as ClosedSet>::parse_label("defmacroo").unwrap_err();
    assert_eq!(rejected.to_string(), "unknown macro definition head: defmacroo");
    assert_eq!(<MacroDefHead as ClosedSet>::SET_LABEL, "macro definition head");
}

#[test]
fn suggest_closest_reaches_the_shared_suggest_metric() {
    // `suggest_closest` must DELEGATE to the crate's one `suggest` metric,
    // not carry its own inline scoring. Pinned by asserting the two agree on
    // the same input — a private reimplementation would drift here.
    //
    // The metric used to live in `tatara-lisp` and this assertion named
    // `tatara_lisp::domain::suggest`. Phase 2 INVERTed that edge (this crate
    // is now a leaf), so it names the local definition. `tatara-lisp`
    // re-exports it, so `tatara_lisp::domain::suggest` is still the same
    // function — it just cannot be referenced from here any more.
    assert_eq!(
        <ChannelKind as ClosedSet>::suggest_closest("Stdou"),
        Some(ChannelKind::Stdout)
    );
    let labels = <ChannelKind as ClosedSet>::labels();
    assert_eq!(tatara_closed_set::suggest("Stdou", &labels), Some("Stdout"));
}

#[test]
fn derived_implementors_satisfy_the_full_well_formedness_contract() {
    assert_closed_set_well_formed::<ChannelKind>();
    assert_closed_set_well_formed::<MacroDefHead>();
}
