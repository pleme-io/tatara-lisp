//! Phase F constructs — the first coverage they have ever had.
//!
//! `#[tatara(domain)]`, `#[tatara(keyword_enum)]` and `#[derive(KeywordSexp)]`
//! were implemented and then never exercised. Measured 2026-08-18 across the
//! fleet: 202 files use `DeriveTataraDomain`, and these three have **zero** use
//! sites anywhere — every hit is this crate's own source, a trait definition, a
//! re-export, or a downstream comment saying "we don't use this here".
//!
//! Implemented-but-uncompiled is a distinct risk from unimplemented: it looks
//! finished from the outside, and the first consumer pays for every gap. This
//! file is that first consumer, extracted from the probe that priced a theme
//! vocabulary built on all three.
//!
//! Two behaviours are pinned here because a downstream design was written
//! against a guess about each, and only one of the guesses was right.

use serde::Deserialize;
use tatara_lisp::{DeriveKeywordSexp, DeriveTataraDomain, KeywordSexp, TataraDomain};

/// Unit-only enum reached from a keyword atom.
///
/// ★ The naming rule, pinned: the derive lowercases the IDENT with **no
/// separator** (`tatara-lisp-derive/src/lib.rs:83`). So a two-word variant
/// `TextStrong` is `:textstrong`, NOT `:text-strong`. Field NAMES take the
/// opposite rule — those go through `snake_to_kebab`, so a field `text_strong`
/// IS the kwarg `:text-strong`. The two conventions differ, and a design that
/// assumes one rule for both produces forms that do not parse.
#[derive(Debug, PartialEq, Eq, Clone, Copy, DeriveKeywordSexp)]
enum Polarity {
    Dark,
    Light,
}

#[derive(Debug, PartialEq, Eq, DeriveTataraDomain)]
#[tatara(keyword = "slot")]
struct Slot {
    name: String,
    hex: String,
}

/// Exercises all three constructs at once, including the `Vec<T>` arm of
/// `#[tatara(domain)]` — which is how a fixed-size table (a 16-slot colour
/// ramp, a set of routes) is expressed at all, given the language has neither
/// a map nor a vector type.
#[derive(Debug, PartialEq, Eq, DeriveTataraDomain)]
#[tatara(keyword = "defprobe")]
struct Probe {
    name: String,
    #[tatara(keyword_enum)]
    polarity: Polarity,
    #[tatara(domain)]
    slots: Vec<Slot>,
    #[tatara(domain)]
    primary: Slot,
}

/// A newtype reaching the border through the `Kind::Deserialize` fall-through,
/// with no hand-written extractor.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
struct Hex(u8, u8, u8);

impl TryFrom<String> for Hex {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        let h = s.strip_prefix('#').unwrap_or(&s);
        if h.len() != 6 {
            return Err(format!("expected 6 hex digits, got {s:?}"));
        }
        let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|e| e.to_string());
        Ok(Hex(b(0)?, b(2)?, b(4)?))
    }
}

const GOOD: &str = r#"
(defprobe
  :name "nord"
  :polarity :dark
  :primary (slot :name "base00" :hex "2e3440")
  :slots ((slot :name "base00" :hex "2e3440")
          (slot :name "base01" :hex "3b4252")))
"#;

fn compile(src: &str) -> tatara_lisp::Result<Probe> {
    let forms = tatara_lisp::read(src)?;
    Probe::compile_from_sexp(&forms[0])
}

#[test]
fn all_three_phase_f_constructs_compile_and_parse() {
    let p = compile(GOOD).expect("probe form must compile");
    assert_eq!(p.name, "nord");
    assert_eq!(p.polarity, Polarity::Dark);
    assert_eq!(p.slots.len(), 2);
    assert_eq!(p.slots[1].name, "base01");
    assert_eq!(p.primary.hex, "2e3440");
}

/// ★ A typo'd KWARG is rejected — with a did-you-mean and the allowed set.
///
/// Worth pinning because the surface notes warn that a mistyped keyword yields
/// an empty `Vec` **reported as success**. That warning describes the manual
/// extraction path; on the DERIVE path the emitted `__TATARA_ALLOWED_KEYWORDS`
/// gate closes it. The distinction matters: a 16-slot table authored with one
/// mistyped key would otherwise render partial and green.
#[test]
fn typod_kwarg_is_rejected_with_a_suggestion() {
    let err = compile(&GOOD.replace(":slots", ":slotz"))
        .expect_err("a typo'd kwarg must not compile")
        .to_string();
    assert!(err.contains("slotz"), "error must name the bad key: {err}");
    assert!(
        err.contains("slots"),
        "error must suggest the intended key: {err}"
    );
}

#[test]
fn unknown_keyword_enum_value_is_rejected() {
    assert!(compile(&GOOD.replace(":dark", ":dusk")).is_err());
}

#[test]
fn missing_required_nested_form_is_rejected() {
    let missing = GOOD
        .lines()
        .filter(|l| !l.contains(":primary"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compile(&missing).is_err());
}

#[test]
fn keyword_sexp_lowercases_the_ident_with_no_separator() {
    assert_eq!(Polarity::Dark.to_keyword(), "dark");
    assert!(Polarity::from_keyword("dark").is_ok());
    assert!(Polarity::from_keyword("dusk").is_err());
}

#[test]
fn newtype_reaches_the_border_via_serde_try_from() {
    let forms = tatara_lisp::read(r#"(slot :name "base00" :hex "2e3440")"#).unwrap();
    let s = Slot::compile_from_sexp(&forms[0]).unwrap();
    let h = Hex::try_from(s.hex).unwrap();
    assert_eq!((h.0, h.1, h.2), (0x2e, 0x34, 0x40));
}

/// ★ THE OPEN HAZARD, pinned as a CHARACTERIZATION test rather than a wish.
///
/// A typo'd STRUCT attribute — `#[tatara(keywords = …)]` for `keyword` — does
/// NOT fail. `parse_nested_meta`'s error is swallowed by a `let _ =`
/// (`tatara-lisp-derive/src/lib.rs:219-241`), so the derive silently falls back
/// to a keyword computed from the STRUCT NAME. Measured: a struct `AttrTypo`
/// annotated `#[tatara(keywords = "deftypo")]` answers to `"defattrtypo"` —
/// neither what the author wrote nor an error.
///
/// Consequence for consumers: the keyword a form answers to cannot be trusted
/// from the attribute alone. Pin it with a literal `assert_eq!(T::KEYWORD, …)`
/// per domain type until the derive rejects unknown sub-keys.
///
/// This test asserts the CURRENT behaviour so the day it is fixed, it fails and
/// says so — rather than the fix landing unnoticed.
mod attribute_typo_is_silent {
    use super::*;

    #[derive(Debug, DeriveTataraDomain)]
    #[tatara(keywords = "deftypo")]
    struct AttrTypo {
        #[allow(dead_code)]
        name: String,
    }

    #[test]
    fn typod_struct_attribute_falls_back_to_the_struct_name() {
        assert_eq!(
            AttrTypo::KEYWORD,
            "defattrtypo",
            "if this fails, the derive changed: either it now honours `keywords`, \
             or it now rejects unknown sub-keys. Both are improvements — update \
             this test and the ceiling it documents."
        );
        assert_ne!(AttrTypo::KEYWORD, "deftypo");
    }
}
