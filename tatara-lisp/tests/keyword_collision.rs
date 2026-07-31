//! The keyword namespace is EXCLUSIVE — one keyword, one type, per process.
//!
//! `register::<T>()` writes into a process-global `HashMap<&'static str, _>`.
//! Before this file existed it did a bare `insert`, discarded the displaced
//! handler, and documented the behaviour as "Idempotent — repeated
//! registrations overwrite". That sentence is true of a type re-registering
//! ITSELF and false of two different types landing on one keyword: the second
//! silently replaced the first, and `lookup` afterwards returned a handler that
//! compiled a DIFFERENT struct than the caller's keyword named.
//!
//! Measured 2026-07-31 over the pleme-io tree at `~/code/github/pleme-io`
//! (denominator: 349 `#[tatara(keyword = "…")]` declarations across the repos
//! checked out there, `rg --no-ignore` per repo; a bare `rg` from the org root
//! returns 0 because the org root carries a blanket `*` .gitignore): 318
//! distinct keywords, of which 20 are declared by two or more DIFFERENT crates
//! — `defagent defbind defcommand defeffect defenv defescriba defflag defflake
//! defhook defkeymap defmark defmaterial defnotify defoption defplugin defpoint
//! defrouting defsession defsystem deftheme`. That census is a snapshot of one
//! checkout on one date, not a fleet-wide guarantee; re-measure before quoting
//! it. Eight of the twenty have both sides calling `register()` in shipped
//! code, and `defplugin` has three declarers, two of which
//! (`escriba-config`, `escriba-lisp`) are linked into the SAME escriba binary.
//!
//! These tests pin the replacement contract:
//!   * first writer wins — an incumbent is never displaced;
//!   * the loser is TOLD, by a typed `KeywordCollision`, not by silence;
//!   * a type re-registering itself is still idempotent `Ok(())`.

use tatara_lisp::Sexp;
use tatara_lisp::domain::{KeywordCollision, TataraDomain, lookup, register};

// Two genuinely different types that both claim one keyword — the shape the
// fleet census found 20 instances of. `compile_from_args` returns a
// distinguishable payload so the test can prove WHICH type the registry
// dispatches to, rather than merely that some handler is present.

#[derive(serde::Serialize)]
struct Incumbent;

impl TataraDomain for Incumbent {
    const KEYWORD: &'static str = "defcolisao";
    fn compile_from_args(_args: &[Sexp]) -> tatara_lisp::Result<Self> {
        Ok(Self)
    }
}

#[derive(serde::Serialize)]
struct Challenger;

impl TataraDomain for Challenger {
    const KEYWORD: &'static str = "defcolisao";
    fn compile_from_args(_args: &[Sexp]) -> tatara_lisp::Result<Self> {
        Ok(Self)
    }
}

/// A distinct keyword, so the idempotency test cannot be satisfied by the
/// collision test's leftovers (the registry is process-global and the two
/// tests share it).
#[derive(serde::Serialize)]
struct Solo;

impl TataraDomain for Solo {
    const KEYWORD: &'static str = "defcolisaosolo";
    fn compile_from_args(_args: &[Sexp]) -> tatara_lisp::Result<Self> {
        Ok(Self)
    }
}

#[test]
fn a_colliding_registration_is_rejected_and_the_incumbent_survives() {
    // Ordering note: cargo runs the tests in this binary on threads sharing
    // one registry, so this test owns `defcolisao` outright and the other two
    // use their own keywords. Nothing here depends on which test runs first.
    assert_eq!(
        register::<Incumbent>(),
        Ok(()),
        "the first writer for a free keyword must be accepted"
    );

    let verdict = register::<Challenger>();
    let collision = verdict.expect_err(
        "a second, DIFFERENT type claiming a taken keyword must be rejected, \
         not silently swapped in",
    );

    assert_eq!(collision.keyword, "defcolisao");
    assert!(
        collision.incumbent.ends_with("Incumbent"),
        "the collision must name who already holds the keyword; got {}",
        collision.incumbent
    );
    assert!(
        collision.challenger.ends_with("Challenger"),
        "the collision must name who was turned away; got {}",
        collision.challenger
    );

    // The load-bearing half. A rejection that still mutated the registry would
    // be a worse defect than the silent overwrite, because it would report the
    // opposite of what it did.
    let handler = lookup("defcolisao").expect("the incumbent handler must still be registered");
    assert_eq!(handler.keyword, "defcolisao");
    assert_eq!(
        handler.owner,
        std::any::type_name::<Incumbent>(),
        "lookup must still dispatch to the FIRST registrant"
    );
}

#[test]
fn re_registering_the_same_type_stays_idempotent() {
    // The documented idempotency is real and load-bearing: several crates call
    // `Foo::register()` from more than one entry point (a `register_all()`
    // seed fn plus a lazy path). Only cross-TYPE collision is the defect.
    assert_eq!(register::<Solo>(), Ok(()));
    assert_eq!(
        register::<Solo>(),
        Ok(()),
        "a type re-registering itself must remain a no-op success"
    );
}

#[test]
fn a_collision_renders_a_message_naming_both_sides() {
    let collision = KeywordCollision {
        keyword: "defplugin",
        incumbent: "escriba_config::PluginSpec",
        challenger: "escriba_lisp::plugin::PluginSpec",
    };
    let rendered = collision.to_string();
    assert!(rendered.contains("defplugin"), "{rendered}");
    assert!(rendered.contains("escriba_config::PluginSpec"), "{rendered}");
    assert!(
        rendered.contains("escriba_lisp::plugin::PluginSpec"),
        "{rendered}"
    );
}
