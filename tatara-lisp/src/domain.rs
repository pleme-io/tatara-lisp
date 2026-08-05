//! `TataraDomain` — a Rust type authorable as a Lisp `(<keyword> :k v …)` form.
//!
//! Apply `#[derive(TataraDomain)]` (from `tatara-lisp-derive`) and a plain
//! struct gains a full Lisp compiler: keyword dispatch, kwarg parsing, typed
//! field extraction.
//!
//! Also exposes a `DomainRegistry` + `linkme`-free `register_domain!` macro
//! so any crate that derives `TataraDomain` can auto-register itself; the
//! dispatcher then looks up unknown top-level forms by keyword at runtime.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::de::DeserializeOwned;

use crate::ast::{Atom, Sexp};
use crate::error::{
    ExpectedKwargShape, KwargPath, LispError, NumericLiteral, NumericWidth, Result, SexpShape,
    SexpWitness,
};

/// Phase F: a Rust type (typically a unit-only enum) whose variants map to
/// a single Lisp keyword atom — e.g., `Role::Master` ↔ `:master`. Used by
/// `#[derive(TataraDomain)]` fields with `#[tatara(keyword_enum)]`. Derive
/// via `#[derive(KeywordSexp)]` from `tatara-lisp-derive`.
pub trait KeywordSexp: Sized {
    /// Parse `s` (the keyword name without the leading `:`) into Self.
    fn from_keyword(s: &str) -> Result<Self>;
    /// The keyword name (without leading `:`) for this variant.
    fn to_keyword(self) -> &'static str;
}

/// A Rust type compilable from a Lisp form.
pub trait TataraDomain: Sized {
    /// The Lisp keyword (e.g., `"defmonitor"`).
    const KEYWORD: &'static str;

    /// Parse the argument list (everything after the keyword) into Self.
    fn compile_from_args(args: &[Sexp]) -> Result<Self>;

    /// Parse a complete form; validates the head symbol matches `KEYWORD`.
    fn compile_from_sexp(form: &Sexp) -> Result<Self> {
        let list = form
            .as_list()
            .ok_or_else(|| not_a_list_form_err(Self::KEYWORD))?;
        // The two sub-modes of "head can't be projected to a symbol" — empty
        // list (`first()` is `None`) vs. present-but-not-a-symbol
        // (`as_symbol()` is `None`) — share ONE structural variant
        // (`MissingHeadSymbol { keyword, got }`) but bind to distinct
        // `got` payloads (`None` vs. `Some(<sexp display>)`). This lets
        // an authoring tool render "your form is empty" vs. "your
        // form's head is `5`, not a symbol" without re-parsing the
        // source — the legacy `Compile`-shaped diagnostic collapsed
        // both into one message.
        let head_sexp = list
            .first()
            .ok_or_else(|| missing_head_err(Self::KEYWORD, None))?;
        let head = head_sexp
            .as_symbol()
            .ok_or_else(|| missing_head_err(Self::KEYWORD, Some(head_sexp.witness())))?;
        if head != Self::KEYWORD {
            return Err(head_mismatch(Self::KEYWORD, head.to_string()));
        }
        Self::compile_from_args(&list[1..])
    }
}

// ── compile_from_sexp diagnostics — the form-shape gate primitives ─
//
// `compile_from_sexp` (the trait default) gates every `TataraDomain`
// invocation that takes a complete `(KEYWORD …)` form: ProcessSpec,
// MonitorSpec, AlertPolicySpec, every hand-written impl. Three failure
// modes — not a list, missing head symbol, wrong head — used to be
// inline `LispError::Compile { form: KEYWORD.to_string(), message: …}`
// triples in the trait default. The three-times-rule signal
// (THEORY.md §VI.1) calls for one named primitive per shape; these
// are them.
//
// All three are now structural: `not_a_list_form_err` returns
// `LispError::NotAListForm`, `missing_head_err` returns
// `LispError::MissingHeadSymbol { keyword, got }` (`got: None` for
// empty list, `got: Some(<sexp display>)` for present-but-not-symbol),
// and `head_mismatch` returns `LispError::HeadMismatch`. Each carries
// its distinguishing data (the offending head's display projection,
// the keyword) as first-class variant fields so authoring tools
// pattern-match structurally instead of substring-grepping the
// rendered message. The entire `compile_from_sexp` rejection chain
// — bare-atom → empty/not-symbol head → wrong-keyword head — is
// closed: every distinct typed-entry rejection at the form-shape
// gate binds to ONE structural variant of `LispError`.

/// `T::compile_from_sexp` was passed something that isn't a list.
/// One named primitive every TataraDomain impl shares — returns the
/// dedicated `LispError::NotAListForm { keyword }` variant so
/// authoring surfaces (REPL, LSP, `tatara-check`) bind to the
/// first-class `keyword` field instead of substring-parsing the
/// rendered message. Display matches the legacy `Compile`-shaped
/// diagnostic byte-for-byte (`"compile error in {keyword}: expected
/// list form"`), so existing `format!("{err}").contains("expected
/// list form")` assertions pass unchanged.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform. The legacy
/// `Compile { form, message }` shape required consumers to
/// pattern-match on `message == "expected list form"` to recognize
/// this specific gate (versus the sibling `missing head symbol`
/// gate, which produces the same `Compile` shape with a different
/// message). After this lift the discriminator is the variant
/// itself — a regression that drifts the message string can no
/// longer drift the gate's identity. THEORY.md §II.1 invariant 1 —
/// typed entry; a non-list form is exactly the failure mode the
/// typed-entry gate exists to reject, and the gate's identity is
/// now load-bearing in the type system.
#[must_use]
pub fn not_a_list_form_err(keyword: &'static str) -> LispError {
    LispError::NotAListForm { keyword }
}

/// `T::compile_from_sexp` was passed `()` or a list whose first
/// element isn't a symbol — there's nothing to dispatch on. One named
/// primitive every `TataraDomain` impl shares; returns the dedicated
/// `LispError::MissingHeadSymbol { keyword, got }` variant so authoring
/// surfaces (REPL, LSP, `tatara-check`) bind to the first-class
/// `keyword` and `got` fields instead of substring-parsing the
/// rendered message. `got: None` for the empty-list case (`()`),
/// `got: Some(SexpWitness)` for the present-but-not-symbol case
/// (`(5 …)`, `(:foo …)`, `("x" …)`, `((nested) …)`) — the legacy
/// `Compile`-shaped diagnostic collapsed both into one message; this
/// builder bifurcates them structurally so the renderable detail
/// names which sub-mode fired. The `Some` arm carries the typed
/// joint identity (`SexpShape` + `Sexp::Display`) routed through
/// `sexp_witness(_)` so authoring tools that want to surface a
/// structural autofix — "you wrote `:foo` at the head slot where a
/// symbol was expected (did you mean `foo`?)" — bind on
/// `got.shape == SexpShape::Keyword` directly, no substring-grep on
/// the rendered display required.
///
/// Display matches the legacy `Compile`-shaped diagnostic byte-for-
/// byte for the prefix (`"compile error in {keyword}: missing head
/// symbol"`); the structural detail is appended in a parenthetical
/// (`(empty list)` for `None`, `(got {g})` for `Some(g)`), parallel
/// to how `RestParamMissingName` appends `(rest marker at position
/// {n}, {got|none provided})` and how `SpliceOutsideList` appends
/// `(got ,@{got})`. The `{g}` slot flows through `SexpWitness::Display`,
/// which writes only the `display` field, so existing
/// `format!("{err}").contains("missing head symbol")` assertions pass
/// unchanged.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform. The legacy
/// `Compile { form, message }` shape required consumers to
/// pattern-match on `message == "missing head symbol"` to recognize
/// this specific gate (versus the sibling `expected list form` and
/// head-mismatch gates, which produced different `message` strings
/// in the same `Compile` shape). After this lift the discriminator
/// is the variant itself — a regression that drifts the message
/// string can no longer drift the gate's identity, AND the two
/// distinct sub-modes (empty vs. present-but-not-symbol) are
/// structurally addressable. THEORY.md §II.1 invariant 1 — typed
/// entry; an empty form / non-symbol-head form is exactly the
/// failure mode the typed-entry gate exists to reject, and the
/// gate's identity is now load-bearing in the type system.
#[must_use]
pub fn missing_head_err(keyword: &'static str, got: Option<SexpWitness>) -> LispError {
    LispError::MissingHeadSymbol { keyword, got }
}

/// Structural head-mismatch builder. Returns the dedicated
/// `LispError::HeadMismatch` variant so authoring surfaces (REPL, LSP,
/// `tatara-check`) bind to first-class `keyword`/`got` fields instead
/// of substring-parsing the rendered message. Display matches the
/// legacy `Compile`-shaped diagnostic byte-for-byte, so existing
/// `format!("{err}").contains("expected ({KEYWORD}")` assertions pass
/// unchanged.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform. A diagnostic
/// whose `got` is embedded in a free-form message is structurally
/// incomplete; an authoring surface that wants to render
/// "did-you-mean" suggestions on the offending head must re-parse
/// the message. After this lift the slot exists in the variant's
/// data shape itself.
#[must_use]
pub fn head_mismatch(keyword: &'static str, got: String) -> LispError {
    LispError::HeadMismatch { keyword, got }
}

/// The substrate-wide [`TataraDomain`] well-formedness testkit — closes
/// the four typed-entry rejection gates on the trait's default
/// [`TataraDomain::compile_from_sexp`], three [`TataraDomain::KEYWORD`]
/// grammar invariants, AND the reader round-trip theorem at ONE call
/// every implementor's test module reaches for.
///
/// Peer of [`crate::closed_set::assert_closed_set_well_formed`] on the
/// sibling [`crate::ClosedSet`] contract — after this lift both
/// homoiconic-authoring contracts (the closed-set enum idiom AND the
/// derived-domain idiom) carry ONE substrate-wide structural checker
/// each, and every downstream implementor's test module reduces to a
/// single-line invocation instead of re-deriving the invariants
/// per-implementor.
///
/// ## The four `compile_from_sexp` rejection gates
///
///   1. `NotAListForm { keyword }` on a bare atom — the typed-entry
///      gate rejects the form-shape mismatch before descending into
///      the list.
///   2. `MissingHeadSymbol { keyword, got: None }` on the empty list
///      `()` — `list.first()` returns `None`, there's no head to
///      project.
///   3. `MissingHeadSymbol { keyword, got: Some(_) }` on a list whose
///      first element is not a symbol — `list.first().as_symbol()`
///      returns `None`, and the offending element's typed identity
///      threads into the `got` slot.
///   4. `HeadMismatch { keyword, got }` on a list headed by a symbol
///      other than `T::KEYWORD` — the substring-free structural
///      discriminator.
///
/// ## The three `KEYWORD` grammar invariants
///
///   5. `KEYWORD` is non-empty — a keyword-less form cannot be
///      dispatched.
///   6. `KEYWORD` classifies as [`Atom::Symbol`] through the substrate's
///      typed-entry classifier [`Atom::from_lexeme`] — the ONE
///      projection every bare-atom lexeme routes through inside the
///      reader's parse arm. Subsumes the pre-lift "no leading ASCII
///      digit" heuristic (a KEYWORD `"42"` decodes as [`Atom::Int`],
///      `"1.5"` as [`Atom::Float`]) AND catches the two shapes the
///      pre-lift check silently accepted: a leading `:` (KEYWORD
///      `":foo"` decodes as [`Atom::Keyword`]) and the two boolean
///      literals (`"#t"` / `"#f"` decode as [`Atom::Bool`]) — none of
///      which the trait's `as_symbol()` head-match would fire on. Binds
///      the invariant to the substrate's typed reader-classifier
///      algebra so a future seventh [`Atom`] variant (e.g. `Char` for
///      `#\x` reader syntax, `Bigint` for arbitrary-precision integers)
///      strengthens the check ONCE at [`Atom::from_lexeme`] rather than
///      re-heuristicing per implementor's test module.
///   7. `KEYWORD` contains no [`Sexp::is_bare_atom_boundary`] char —
///      the ONE typed projection on the outer [`Sexp`] algebra that
///      names "this char breaks the reader's bare-atom accumulator."
///      Subsumes the pre-lift "no ASCII whitespace" heuristic (via
///      `char::is_whitespace()` covering the Unicode whitespace surface
///      the reader also splits on — NBSP `\u{00A0}`, ideographic space
///      `\u{3000}`, and every other codepoint the pre-lift ASCII-only
///      check silently accepted) AND catches the seven non-whitespace
///      terminators the pre-lift check ignored:
///      [`Sexp::LIST_OPEN`] `(`, [`Sexp::LIST_CLOSE`] `)`,
///      [`crate::ast::QuoteForm::QUOTE_LEAD`] `'`,
///      [`crate::ast::QuoteForm::QUASIQUOTE_LEAD`] `` ` ``,
///      [`crate::ast::QuoteForm::UNQUOTE_LEAD`] `,`,
///      [`Atom::STR_DELIMITER`] `"`, [`Sexp::COMMENT_LEAD`] `;` — every
///      char that would tokenize a KEYWORD like `"def(x"` / `"def;x"` /
///      `"def\"x"` into TWO tokens, breaking the trait's head-match
///      structurally. Binds the invariant to the substrate's typed
///      reader-boundary algebra so a future eighth outer-dispatch
///      category (e.g. `#|…|#` block-comment lead byte) strengthens the
///      check ONCE at [`Sexp::is_bare_atom_boundary`] rather than
///      per-implementor re-derivation.
///
/// ## The round-trip theorem
///
///   8. `read(KEYWORD)` produces exactly one form, and that form's
///      [`Sexp::as_symbol`] projection returns `Some(KEYWORD)`. This is
///      the STRUCTURAL condition the trait's default
///      [`TataraDomain::compile_from_sexp`] head-match depends on: the
///      reader tokenizes the head slot, projects through
///      [`Atom::from_lexeme`], and the head-match calls `as_symbol()`
///      on the resulting [`Sexp`]. If the round-trip holds, the
///      head-match fires on the intended keyword; if it fails, no
///      other invariant matters. Invariants (6) and (7) together
///      *entail* this theorem — (6) closes the classifier axis,
///      (7) closes the tokenizer axis — so a KEYWORD that passes both
///      structural checks always passes the round-trip; pinning the
///      theorem explicitly closes the LOOP at the verification site
///      and catches drift outside the closed-set structural surface
///      (e.g. a future reader-level input transformation that couldn't
///      be reduced to either axis).
///
/// A hand-written implementor that overrides
/// [`TataraDomain::compile_from_sexp`] and drifts any of the four gates
/// from the substrate-wide structural variants surfaces here rather
/// than as a mystery integration failure downstream — the same posture
/// [`crate::closed_set::assert_closed_set_well_formed`] takes on the
/// override-prone `parse_label` / `parse_label_with_hint` /
/// `labels_joined` axes.
///
/// ## Usage
///
/// ```ignore
/// #[test]
/// fn my_spec_is_well_formed_tatara_domain() {
///     tatara_lisp::assert_tatara_domain_well_formed::<MySpec>();
/// }
/// ```
///
/// ## Theory grounding
///
/// THEORY.md §II.1 invariant 1 — typed entry; the four structural
/// gates ARE the typed-entry boundary of the derived-domain idiom, and
/// the testkit makes their identity load-bearing at the per-implementor
/// test surface.
///
/// THEORY.md §V.1 — knowable platform; the four structural rejections
/// were previously re-derived per implementor with
/// `matches!(err, LispError::NotAListForm { ... })` scaffolds. The
/// testkit collapses the scaffolds onto ONE substrate entry so future
/// implementors inherit the contract by calling one line — mirrors the
/// `assert_closed_set_well_formed` posture that closed the closed-set
/// enum idiom's 36+ per-implementor test modules onto ONE checker.
///
/// THEORY.md §VI.1 — generation over composition; the four gate
/// primitives ([`not_a_list_form_err`], [`missing_head_err`],
/// [`head_mismatch`]) already compose the structural rejections at the
/// GENERATION site. This testkit closes the LOOP at the VERIFICATION
/// site so the two ends of the substrate meet at ONE structural
/// witness — every implementor's test module inherits both halves
/// through ONE call rather than restating the four `matches!` arms
/// per-implementor.
#[track_caller]
pub fn assert_tatara_domain_well_formed<T>()
where
    T: TataraDomain,
{
    let type_name = core::any::type_name::<T>();
    let keyword = T::KEYWORD;

    // (1) — KEYWORD is non-empty. A keyword-less form has no head
    // symbol for the dispatch to key on; the trait's contract is
    // structurally degenerate without a discriminating lexeme.
    assert!(
        !keyword.is_empty(),
        "{type_name}: TataraDomain::KEYWORD is empty — the head symbol has no lexeme to dispatch on",
    );

    // (2) — KEYWORD classifies as `Atom::Symbol` through the substrate's
    // typed-entry classifier `Atom::from_lexeme`. The reader routes every
    // bare-atom lexeme through this ONE projection; anything that decodes
    // as `Bool` / `Keyword` / `Int` / `Float` / `Str` never reaches the
    // trait's head-match as a symbol. Subsumes the pre-lift "no leading
    // ASCII digit" heuristic (`"42"` → `Int`, `"1.5"` → `Float`) AND
    // catches the two shapes the pre-lift check silently accepted:
    // `":foo"` → `Keyword` and `"#t"` / `"#f"` → `Bool`. Binding to the
    // substrate's classifier means a future seventh `Atom` variant
    // (`Char`, `Bigint`) strengthens the check ONCE.
    match Atom::from_lexeme(keyword) {
        Atom::Symbol(s) if s == keyword => {}
        classified => panic!(
            "{type_name}: KEYWORD {keyword:?} classifies as {classified:?} via Atom::from_lexeme — the Lisp reader would not project the head as a symbol at the trait's head-match",
        ),
    }

    // (3) — KEYWORD contains no `Sexp::is_bare_atom_boundary` char. The
    // substrate's typed reader-boundary projection covers BOTH the
    // Unicode-whitespace surface (via `char::is_whitespace`) AND the
    // seven non-whitespace terminators (`(` `)` `'` `` ` `` `,` `"` `;`)
    // that would tokenize the KEYWORD into two tokens, breaking the
    // trait's head-match structurally. Subsumes the pre-lift
    // "no ASCII whitespace" heuristic; binding to the substrate's typed
    // reader-boundary algebra means a future eighth outer-dispatch
    // category (`#|` block-comment lead) strengthens the check ONCE.
    if let Some(ch) = keyword.chars().find(|&c| Sexp::is_bare_atom_boundary(c)) {
        panic!(
            "{type_name}: KEYWORD {keyword:?} contains reader-boundary char {ch:?} (Sexp::is_bare_atom_boundary → true) — the Lisp reader would split it into multiple tokens, breaking the head-match structurally",
        );
    }

    // (4) — a bare-atom form rejects with `NotAListForm { keyword }`.
    // The typed-entry gate rejects the form-shape mismatch before
    // descending into the list's head; the variant carries the keyword
    // as structural data so authoring surfaces bind on
    // `LispError::NotAListForm { keyword }` rather than substring-
    // parsing the rendered message.
    let bare_atom = Sexp::int(0);
    match T::compile_from_sexp(&bare_atom) {
        Err(LispError::NotAListForm { keyword: k }) => assert_eq!(
            k, keyword,
            "{type_name}: NotAListForm.keyword {k:?} drifted from T::KEYWORD {keyword:?}",
        ),
        Ok(_) => panic!(
            "{type_name}: compile_from_sexp accepted a bare-atom form — the typed-entry gate would let a non-list form silently reach the kwargs decoder",
        ),
        Err(other) => panic!(
            "{type_name}: compile_from_sexp on a bare-atom form emitted {other:?}, expected LispError::NotAListForm {{ keyword: {keyword:?} }}",
        ),
    }

    // (5) — the empty list `()` rejects with
    // `MissingHeadSymbol { keyword, got: None }`. `list.first()`
    // returns `None`, so no head-witness is threaded through the
    // rejection — the `got: None` arm names the empty-list sub-mode
    // structurally.
    let empty_list = Sexp::List(Vec::new());
    match T::compile_from_sexp(&empty_list) {
        Err(LispError::MissingHeadSymbol {
            keyword: k,
            got: None,
        }) => assert_eq!(
            k, keyword,
            "{type_name}: MissingHeadSymbol.keyword {k:?} drifted from T::KEYWORD {keyword:?} on the empty-list arm",
        ),
        Ok(_) => panic!(
            "{type_name}: compile_from_sexp accepted the empty list `()` — the typed-entry gate would let a headless form silently reach the head-match",
        ),
        Err(other) => panic!(
            "{type_name}: compile_from_sexp on the empty list `()` emitted {other:?}, expected LispError::MissingHeadSymbol {{ keyword: {keyword:?}, got: None }}",
        ),
    }

    // (6) — a list whose head is a non-symbol atom rejects with
    // `MissingHeadSymbol { keyword, got: Some(_) }`. The offending
    // element's typed identity threads through `SexpWitness` into the
    // `got` slot so authoring surfaces can render "your form's head
    // is `0`, an int, not a symbol" without re-parsing the source.
    let non_symbol_head = Sexp::List(vec![Sexp::int(0)]);
    match T::compile_from_sexp(&non_symbol_head) {
        Err(LispError::MissingHeadSymbol {
            keyword: k,
            got: Some(_),
        }) => assert_eq!(
            k, keyword,
            "{type_name}: MissingHeadSymbol.keyword {k:?} drifted from T::KEYWORD {keyword:?} on the non-symbol-head arm",
        ),
        Ok(_) => panic!(
            "{type_name}: compile_from_sexp accepted a form with a non-symbol head — the typed-entry gate would let a numeric-head form silently reach the head-match",
        ),
        Err(other) => panic!(
            "{type_name}: compile_from_sexp on a non-symbol-head form emitted {other:?}, expected LispError::MissingHeadSymbol {{ keyword: {keyword:?}, got: Some(_) }}",
        ),
    }

    // (7) — a symbol-headed list whose head is NOT `T::KEYWORD`
    // rejects with `HeadMismatch { keyword, got }`. The probe symbol
    // is chosen to be lexically distinct from every conceivable
    // canonical keyword across the substrate so no real implementor
    // can accidentally match it. A hard equality assertion rules out
    // the degenerate case where an implementor's KEYWORD collides
    // with the reserved probe.
    let probe = "__assert_tatara_domain_well_formed_probe__";
    assert_ne!(
        keyword, probe,
        "{type_name}: T::KEYWORD collides with the reserved probe {probe:?} — the wrong-head arm cannot rule out an implementor whose KEYWORD equals the probe; rename either side",
    );
    let wrong_head = Sexp::List(vec![Sexp::symbol(probe)]);
    match T::compile_from_sexp(&wrong_head) {
        Err(LispError::HeadMismatch { keyword: k, got }) => {
            assert_eq!(
                k, keyword,
                "{type_name}: HeadMismatch.keyword {k:?} drifted from T::KEYWORD {keyword:?}",
            );
            assert_eq!(
                got, probe,
                "{type_name}: HeadMismatch.got {got:?} drifted from the offending head {probe:?}",
            );
        }
        Ok(_) => panic!(
            "{type_name}: compile_from_sexp accepted a form headed by the reserved probe {probe:?} — the typed-entry gate would let a wrong-head form silently reach the kwargs decoder",
        ),
        Err(other) => panic!(
            "{type_name}: compile_from_sexp on the wrong-head form emitted {other:?}, expected LispError::HeadMismatch {{ keyword: {keyword:?}, got: {probe:?} }}",
        ),
    }

    // (8) — reader round-trip theorem. `read(KEYWORD)` produces exactly
    // one form, and that form's `as_symbol()` projection returns
    // `Some(KEYWORD)`. This is the SUFFICIENT condition invariants
    // (6) + (7) together entail: (6) closes the classifier axis (the
    // token, once assembled, classifies as `Atom::Symbol`), (7) closes
    // the tokenizer axis (the KEYWORD arrives at the classifier as ONE
    // token). Pinning the composition explicitly closes the LOOP at
    // the verification site — a substrate-owned theorem the two
    // structural checks compose into — and catches drift outside the
    // closed-set structural surface (e.g. a future reader-level input
    // transformation that couldn't be reduced to either axis).
    match crate::reader::read(keyword) {
        Ok(forms) if forms.len() == 1 && forms[0].as_symbol() == Some(keyword) => {}
        Ok(forms) => panic!(
            "{type_name}: KEYWORD {keyword:?} did not round-trip through read → as_symbol — read produced {forms:?} (expected one form projecting to Some({keyword:?}))",
        ),
        Err(err) => panic!(
            "{type_name}: KEYWORD {keyword:?} failed to tokenize at all — read returned {err:?}",
        ),
    }
}

// ── kwarg parsing + typed extractors used by the derive macro ──────

pub type Kwargs<'a> = HashMap<String, &'a Sexp>;

/// Parse `:k v :k v …` into a kwargs map. Rejects duplicate keywords so the
/// typed-entry gate fires on `(defX :name "a" :name "b")` instead of silently
/// keeping the last value — same posture `reject_unknown_kwargs` takes for
/// typo'd kwargs. A duplicate is ill-typed input: the author either meant
/// distinct keys (typo) or a list (`:tags ("a" "b")`).
///
/// Odd-length kwargs lists fail with `LispError::OddKwargs { dangling }`,
/// where `dangling` is the offending element's `Sexp::Display` projection
/// — `:query` for a keyword whose value got lost, or the literal form of a
/// stray non-keyword. Naming the dangling element keeps the diagnostic
/// structurally complete instead of merely flagging "odd number"; authoring
/// surfaces (REPL, LSP, `tatara-check`) render the mismatch without
/// re-reading the source.
///
/// Theory anchor: THEORY.md §II.1 invariant 1 — "Typed entry. Ill-typed input
/// errors before the value exists." THEORY.md §V.1 — "knowable platform"
/// requires the diagnostic to name what was passed, not only what was
/// expected.
pub fn parse_kwargs(args: &[Sexp]) -> Result<Kwargs<'_>> {
    let mut kw = HashMap::new();
    let mut i = 0;
    while i + 1 < args.len() {
        let key = args[i].as_keyword().ok_or_else(|| {
            type_mismatch(kwargs_pos_form(i), ExpectedKwargShape::Keyword, &args[i])
        })?;
        if kw.insert(key.to_string(), &args[i + 1]).is_some() {
            return Err(duplicate_kwarg(key));
        }
        i += 2;
    }
    if i < args.len() {
        return Err(LispError::OddKwargs {
            dangling: args[i].to_string(),
        });
    }
    Ok(kw)
}

/// Reject any keyword in `kw` that isn't in `allowed`. Closes the typed-entry
/// hole where typos like `:tthreshold 0.99` would otherwise parse silently
/// with the field unset. Emitted by `#[derive(TataraDomain)]` after
/// `parse_kwargs` so every derived domain rejects unknown kwargs by default.
///
/// When the offending keyword is a near-miss of an allowed kwarg (bounded
/// edit distance via `suggest`), the diagnostic prepends a `did you mean
/// :X?` hint so the operator goes straight to the fix without scanning the
/// allowed-list. The hint is purely additive — `unknown keyword` and the
/// full allowed list still appear — so existing assertions
/// (`msg.contains("unknown keyword")`, `msg.contains(":threshold")`) pass
/// unchanged.
///
/// Returns the structural `LispError::UnknownKwarg { key, hint, allowed }`
/// variant — same posture as the `OddKwargs` / `DuplicateKwarg` /
/// `MissingKwarg` siblings. After this lift every distinct typed-entry
/// kwarg-gate failure mode binds to ONE structural variant of `LispError`,
/// not a `Compile`-shaped substring.
///
/// Theory anchor: THEORY.md §II.1 invariant 1 (typed entry — "Ill-typed input
/// errors before the value exists"); §V.1 ("knowable platform … Render
/// Anywhere" — naming the likely intended keyword is the floor of a
/// constructive diagnostic).
pub fn reject_unknown_kwargs(kw: &Kwargs<'_>, allowed: &[&str]) -> Result<()> {
    for key in kw.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(unknown_kwarg(key, allowed));
        }
    }
    Ok(())
}

/// Parse `:k v :k v …` AND gate the result against a closed allowed-key set —
/// the fused typed-entry kwargs gate. ONE named primitive every
/// `TataraDomain` impl shares for "compile-from-args header": every
/// `#[derive(TataraDomain)]`-generated `compile_from_args` body emitted by
/// `tatara-lisp-derive` begins with this single call, and every hand-
/// written impl in the forge / lattice / tameshi crates that wants the
/// substrate's closed-set kwargs posture binds to ONE function instead of
/// remembering to call [`parse_kwargs`] AND [`reject_unknown_kwargs`] in
/// that order.
///
/// Before this lift the derive emitted the two-call sequence
/// `let kw = parse_kwargs(args)?; reject_unknown_kwargs(&kw, ALLOWED)?;`
/// verbatim at every consumer's `compile_from_args` body — well past the
/// ≥2 PRIME-DIRECTIVE trigger once the fleet's seven-plus
/// `#[derive(TataraDomain)]` consumers (ProcessSpec, EphemeralSpec,
/// MonitorSpec, NotifySpec, AlertPolicySpec, EscalationStep, CompilerSpec,
/// and every future derived domain) inline the same two lines through the
/// proc-macro emitter. The two-call sequence is structurally one
/// operation — "parse the keyword/value run, then assert every key sits
/// in the static allowed-set" — and a regression that drifts ONE
/// consumer's gate from the others (e.g. the derive emits one call but a
/// hand-written impl emits only the other, or a future emitter swaps the
/// order so `reject_unknown_kwargs` runs against an unparsed slice) is
/// the silent typed-entry hole this primitive closes by construction.
///
/// The two stages are composed in the canonical order:
///   1. [`parse_kwargs`] runs first — odd-length input, non-keyword at a
///      key position, and duplicate keys surface as their structural
///      variants ([`LispError::OddKwargs`] / [`LispError::TypeMismatch`]
///      with `form = kwargs_pos_form(i)` / [`LispError::DuplicateKwarg`]).
///   2. Only on `Ok(kw)` does [`reject_unknown_kwargs`] run — keys
///      outside `allowed` surface as [`LispError::UnknownKwarg`] with the
///      typed `hint` / `allowed` slots populated.
///
/// This ordering is structural: `reject_unknown_kwargs` cannot inspect
/// an unparsed `&[Sexp]`, so parse-stage rejection MUST precede
/// reject-stage rejection. A call with BOTH an odd-length tail AND an
/// unknown kwarg surfaces as `OddKwargs` (parse-stage), never as
/// `UnknownKwarg` (reject-stage) — the gate is single-pass and the
/// stages compose in exactly one order. Naming the composition makes
/// that order load-bearing data on the substrate, not a discipline the
/// derive's emit template happens to encode correctly.
///
/// Theory anchor: THEORY.md §II.1 invariant 1 — "Typed entry. Ill-typed
/// input errors before the value exists." The kwargs gate is the
/// typed-entry boundary for every derived domain; closing the gate
/// behind ONE primitive lifts the closed-set posture from the derive's
/// emit template to the substrate's typed surface. THEORY.md §VI.1 —
/// generation over composition; the two-call sequence in the derive's
/// emit template, multiplied across every consumer in the fleet, is
/// well past the three-times rule once the structural shape is named.
/// THEORY.md §V.1 — knowable platform; authoring tools (REPL, LSP,
/// `tatara-check`) that want to surface "this form's kwargs gate
/// rejected because …" bind to the unified primitive's call site
/// instead of guessing which of the two component functions the
/// rejection came from. THEORY.md §II.1 invariant 2 (free middle) —
/// every consumer routes through the SAME composition, so a regression
/// that drifts the order or skips a stage on one path can never reach
/// the substrate's runtime: the type system binds every consumer to
/// the fused primitive's single emission shape.
///
/// Lifetime: the returned [`Kwargs<'a>`] borrows from `args` (the typed
/// alias is `HashMap<String, &'a Sexp>`), so the call site keeps the
/// `&[Sexp]` slice alive for the lifetime of the parsed map — same
/// posture as [`parse_kwargs`]. The fused primitive does not allocate
/// beyond [`parse_kwargs`]'s map: [`reject_unknown_kwargs`] is a pure
/// `O(allowed.len() · kw.len())` scan that returns `Ok(())` on success.
pub fn parse_kwargs_strict<'a>(args: &'a [Sexp], allowed: &[&str]) -> Result<Kwargs<'a>> {
    let kw = parse_kwargs(args)?;
    reject_unknown_kwargs(&kw, allowed)?;
    Ok(kw)
}

/// Structural unknown-kwarg builder. Returns the dedicated
/// `LispError::UnknownKwarg` variant so authoring surfaces (REPL, LSP,
/// `tatara-check`) bind to first-class `key` / `hint` / `allowed`
/// fields instead of substring-parsing the rendered message. Display
/// matches the legacy `Compile { form: kwarg_form(key), message:
/// "unknown keyword (...)" }` rendering byte-for-byte
/// (`"compile error in :{key}: unknown keyword (did you mean :{hint}?;
/// allowed: :a, :b, :c)"` with a hint, `"compile error in :{key}:
/// unknown keyword (allowed: :a, :b, :c)"` without), so existing
/// `msg.contains("unknown keyword")` / `msg.contains(":threshold")` /
/// `msg.contains("did you mean :threshold?")` assertions keep
/// passing.
///
/// Encapsulates the three otherwise-inline steps every unknown-kwarg
/// site shares: (1) ranking the near-miss via `suggest`, (2) sorting
/// the allowed-set lexicographically so two operators on two machines
/// see the same message for the same input — diagnostics are
/// deterministic, (3) materializing the allowed-set as owned
/// `Vec<String>` so the variant lives independent of the call frame
/// and crosses thread boundaries cleanly. A future "registry-aware
/// near-miss for unknown registry-dispatched forms" path
/// (`tatara-check`'s unknown-keyword fallthrough) binds to this
/// helper rather than re-formatting the shape per call site.
///
/// `reject_unknown_kwargs` is the first consumer; hand-written
/// `TataraDomain` impls in the forge / lattice / tameshi crates that
/// don't fit the derive's closed-field-type set bind to the
/// substrate's primitive instead of inline `LispError::Compile { … }`
/// assembly. After this lift `reject_unknown_kwargs` is no longer the
/// last `LispError::Compile { ... }` site in the kwarg-gate's
/// diagnostic surface — every distinct kwarg-gate failure mode is now
/// a structural variant of `LispError`.
///
/// Theory anchor: THEORY.md §V.1 — "Knowable platform … Render
/// Anywhere." A diagnostic whose offending `key` / hint / allowed-set
/// are embedded in a free-form message is structurally incomplete; an
/// authoring surface that wants to render a squiggly under the typo
/// or surface the allowed-set as completions must re-parse the
/// message. After this lift the slots exist in the variant's data
/// shape itself. THEORY.md §II.1 invariant 1 (typed entry) — an
/// unknown kwarg is exactly the failure mode the typed-entry gate
/// exists to reject; naming it structurally is the typed posture for
/// that gate's diagnostic. THEORY.md §VI.1 (generation over
/// composition — one named primitive per structural shape).
#[must_use]
pub fn unknown_kwarg(key: &str, allowed: &[&str]) -> LispError {
    let hint = suggest(key, allowed).map(String::from);
    let mut sorted: Vec<String> = allowed.iter().map(|s| (*s).to_string()).collect();
    sorted.sort();
    LispError::UnknownKwarg {
        key: key.to_string(),
        hint,
        allowed: sorted,
    }
}

/// The typed-entry kwargs-gate's OPTIONAL lookup primitive — `Some(&Sexp)`
/// when `key` is present in `kw`, `None` when absent. ONE named projection
/// on the substrate's `Kwargs<'a>` algebra every optional-kwarg consumer
/// (`extract_optional_atom`, `extract_list`, `extract_optional_via_serde`)
/// routes through, and the sibling [`required`](self::required) composes
/// directly atop it as `optional(kw, key).ok_or_else(|| missing_kwarg(key))`.
/// Before this lift the same `kw.get(key).copied()` projection — turning
/// `Option<&&'a Sexp>` (the raw `HashMap::get` return) into the consumer-
/// shaped `Option<&'a Sexp>` — was inlined verbatim at FOUR sites: once
/// inside `required`'s composition, and once inside each of the three
/// optional consumers' absence-handling preludes. After this lift the
/// projection lives in ONE place; `required` becomes the closed-form
/// composition `optional + ok_or_else(missing_kwarg)`, and the three
/// optional consumers read through `optional(kw, key)` without re-stating
/// the `Option<&&Sexp>` → `Option<&Sexp>` projection at each call site.
///
/// Sibling pair with [`required`](self::required): together the two close
/// the substrate's typed-entry kwargs-LOOKUP surface — `required` is the
/// mandatory-presence path returning `Result<&Sexp>` (absence → typed
/// `LispError::MissingKwarg`); `optional` is the may-be-absent path
/// returning `Option<&Sexp>` (absence → `None`, the consumer decides
/// what default behavior absence triggers — `None` for atoms, empty `Vec`
/// for lists, `Sexp::Nil` for params). The TWO primitives between them
/// cover every consumer's kwargs-lookup posture; a third would be a
/// structural extension the type system would surface at every call site.
/// The composition `required = optional + ok_or_else(missing_kwarg)` is
/// the structural identity binding the two — `required(kw, key)` and
/// `optional(kw, key).ok_or_else(|| missing_kwarg(key))` are
/// observationally identical, and naming the composition makes the
/// identity a substrate-owned theorem rather than a hand-inlined
/// duplication discipline four sites had to keep in lockstep.
///
/// The returned `&'a Sexp` carries the SAME lifetime contract as
/// [`required`](self::required)'s `Ok(&'a Sexp)` — the projection borrows
/// from the kwargs map's value slot via `.copied()`, so the optional
/// consumers can hold the reference through their absence-arm match
/// without an intermediate clone. `'a` is the outer borrow lifetime
/// (mirroring `required`); the inner `'_` is free so call sites with
/// `Kwargs<'a>` (the typical `parse_kwargs` output binding) and
/// `Kwargs<'static>` (a future static-bound shape) both type-check
/// uniformly.
///
/// Theory anchor: THEORY.md §VI.1 — generation over composition; four
/// inline copies of one structural projection past the three-times rule
/// once the structural shape is named. THEORY.md §V.1 — knowable
/// platform; the substrate's typed-entry kwargs-lookup surface is now
/// the named PAIR `{required, optional}` — authoring tools (REPL, LSP,
/// `tatara-check`) that want to surface "this domain reads kwarg X as
/// optional" bind to the `optional` primitive's signature, not the
/// HashMap-level `get` chain. THEORY.md §II.1 invariant 1 — typed entry;
/// the kwargs-lookup gate's two postures (required vs. optional) are
/// now structurally named, so a future fourth posture (e.g. "required
/// with non-empty constraint") extends the pair as a peer rather than
/// silently piggybacking on the inlined `get(key).copied()` chain.
/// THEORY.md §II.1 invariant 2 — free middle; the typed-entry kwargs
/// gate's lookup shape is uniform across every derived domain (and
/// every hand-written `TataraDomain` impl), so a future emitter that
/// wants to instrument the lookup (a span-aware lookup, a debug-mode
/// lookup logger) wraps ONE function rather than four inline sites.
#[must_use]
pub fn optional<'a>(kw: &'a Kwargs<'_>, key: &str) -> Option<&'a Sexp> {
    kw.get(key).copied()
}

/// The typed-entry kwargs-gate's REQUIRED lookup primitive — `Ok(&Sexp)`
/// when `key` is present in `kw`, `Err(LispError::MissingKwarg)` when
/// absent. Composes [`optional`](self::optional) (the may-be-absent
/// lookup) with [`missing_kwarg`](self::missing_kwarg) (the canonical
/// rejection on absence) so the substrate's typed-entry kwargs-lookup
/// surface is named as the PAIR `{required, optional}` with `required`
/// expressed as the closed-form composition of its two sibling
/// primitives. Sibling pair documented in [`optional`](self::optional).
pub fn required<'a>(kw: &'a Kwargs<'_>, key: &str) -> Result<&'a Sexp> {
    optional(kw, key).ok_or_else(|| missing_kwarg(key))
}

/// Canonical typed `form:` value for a kwarg-level `LispError::TypeMismatch`.
/// Every typed-entry diagnostic that names a kwarg (`required`, `type_err`,
/// `deserialize_err`, the duplicate-keyword paths in `parse_kwargs` and
/// `sexp_to_json`, the unknown-keyword path in `reject_unknown_kwargs`,
/// the non-list path in `extract_vec_via_serde`) routes through this one
/// helper, so authoring surfaces (REPL, LSP, `tatara-check`) bind to a
/// single named primitive rather than seven inline `format!(":{key}")`
/// copies.
///
/// Returns the typed `crate::error::KwargPath::Named(key.to_string())` value
/// directly — consumers feed it into `LispError::TypeMismatch.form: KwargPath`
/// where it is structurally bound via pattern-match (`KwargPath::Named(_)`),
/// not substring-matched. The canonical `:<key>` literal lives in ONE place
/// (`KwargPath`'s Display match arm) alongside its sibling shapes
/// `kwarg_item_form` / `kwargs_pos_form`, so a typo in any of the three
/// can never drift independent of the others.
///
/// Theory anchor: THEORY.md §VI.1 — "Generation over composition.
/// Three-times rule: when a pattern repeats three times, extract an
/// archetype/backend/synthesizer and generate from it." Seven inline
/// copies in one module is the textbook signal. THEORY.md §V.1 —
/// knowable platform; the typed `KwargPath` enum encodes the closed set
/// of three reachable path shapes at the type level so authoring tools
/// bind to path-shape identity rather than substring-matching the
/// rendered prefix. THEORY.md §II.1 invariant 1 (typed entry) — the
/// kwargs-path identity is now load-bearing data on the variant rather
/// than a projection-to-String.
#[must_use]
pub fn kwarg_form(key: &str) -> crate::error::KwargPath {
    crate::error::KwargPath::named(key)
}

/// Canonical `form:` label for a failure inside the Nth item of a
/// list-typed kwarg — `:steps[1]` when the second item of `:steps` fails
/// to deserialize, `:tags[2]` when the third tag isn't a string. The
/// substrate names the item-path so the operator sees both *which kwarg*
/// and *which element* misfired without re-counting from the source.
///
/// Frontier inspiration: JSON Pointer (`/steps/1`) and jq path
/// expressions — lossless paths through value projections so downstream
/// tooling (LSP underlines, structural rewrites) bind to the path
/// instead of parsing the diagnostic message. Translation through
/// pleme-io primitives: the surface syntax authors already write
/// (`:<key>` + `[idx]`), no new error variant, no new IR layer. When a
/// future run gives `Sexp` source spans, the indexed form gains a
/// position the same way `kwarg_form` will — one helper, every consumer
/// inherits.
///
/// Theory anchor: THEORY.md §V.1 — "Knowable platform … Render
/// Anywhere." A diagnostic that names the kwarg but loses the item index
/// is structurally incomplete; the path completes it.
///
/// Returns the typed `crate::error::KwargPath::Item { key, idx }` value
/// directly — consumers feed it into `LispError::TypeMismatch.form: KwargPath`
/// where it is structurally bound via pattern-match (`KwargPath::Item { .. }`),
/// not substring-matched. The canonical `:<key>[<idx>]` literal lives in ONE
/// place alongside `kwarg_form` / `kwargs_pos_form`. See `kwarg_form` for the
/// typed-enum's role.
#[must_use]
pub fn kwarg_item_form(key: &str, idx: usize) -> crate::error::KwargPath {
    crate::error::KwargPath::item(key, idx)
}

/// Canonical `form:` label for a kwargs-list slot whose key position is
/// not yet known — the slot itself failed the
/// "this-position-must-be-a-keyword" gate, so there is no `:<key>` to
/// hang the path off. Renders `kwargs[<idx>]` — parallel to
/// `kwarg_item_form`'s `:<key>[<idx>]` shape, rooted at the kwargs
/// slice rather than at a named kwarg.
///
/// Used by `parse_kwargs` to label the structural type-mismatch when
/// the element at an even position isn't a `Sexp::Atom(Keyword(_))`.
/// Pairing this label with the existing `LispError::TypeMismatch`
/// variant (`expected: "keyword"`, `got: sexp_type_name(_)`) means
/// authoring surfaces (REPL, LSP, `tatara-check`) bind to ONE variant
/// identity for every typed-entry mismatch — `:<key>` for kwarg-level
/// failures, `:<key>[<idx>]` for per-item failures, and now
/// `kwargs[<idx>]` for not-a-keyword-yet failures. When a future run
/// gives `Sexp` source spans, the slot-form gains a position the same
/// way `kwarg_form` / `kwarg_item_form` will — one helper, every
/// consumer inherits.
///
/// Theory anchor: THEORY.md §VI.1 (generation over composition — the
/// fourth `form:`-label primitive after `kwarg_form`,
/// `kwarg_item_form`, and the registry-keyword path; one helper per
/// distinct path shape so the substrate's diagnostic surface stays
/// structurally complete).
///
/// Returns the typed `crate::error::KwargPath::Slot(idx)` value directly —
/// consumers feed it into `LispError::TypeMismatch.form: KwargPath` where it
/// is structurally bound via pattern-match (`KwargPath::Slot(_)`), not
/// substring-matched. The canonical `kwargs[<idx>]` literal lives in ONE
/// place alongside `kwarg_form` / `kwarg_item_form`. See `kwarg_form` for
/// the typed-enum's role.
#[must_use]
pub fn kwargs_pos_form(idx: usize) -> crate::error::KwargPath {
    crate::error::KwargPath::Slot(idx)
}

/// Typed projection of a `Sexp`'s outermost shape into the closed-set
/// `SexpShape` enum — the twelve reachable shapes the reader can produce.
/// Used by the typed extractors to thread the observed shape into
/// `LispError::TypeMismatch.got: SexpShape` /
/// `LispError::NamedFormNonSymbolName.got: SexpShape` so a typed-entry
/// gate's rejection-shape identity is load-bearing data in the type
/// system, not a `&'static str` projection at the helper boundary.
/// Consumers (REPL, LSP, `tatara-check`) pattern-match on
/// `SexpShape::Int` etc. directly rather than substring-matching the
/// rendered `got` literal.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform. An error that names
/// only the expected side leaves the operator to guess what was passed;
/// naming both is the floor of constructive diagnostics. The typed
/// projection extends that posture: not just naming both sides, but
/// encoding the observed shape's identity as a TYPE so a regression that
/// drifts the label becomes a compile error, not a runtime substring
/// drift. When a future run gives `Sexp` source spans, this helper is
/// the single site that learns to thread `got Y at <pos>`; today's call
/// sites pick up the span automatically.
/// Free-function delegate to the [`Sexp::shape`] inherent method on the
/// `Sexp` algebra. Retained for backwards compatibility with consumers
/// that import this helper by name (no callers reach in through the
/// module path post-lift); the inherent method is the canonical site
/// for the (Sexp variant, SexpShape variant) projection family —
/// `Atom::kind().sexp_shape()` (atomic axis), `as_quote_form().map(|(qf,
/// _)| qf.sexp_shape())` (quote-family axis), with `Nil` / `List`
/// arms projecting to their own `SexpShape` variants directly. See
/// [`Sexp::shape`]'s docstring for the closed-set composition law and
/// the THEORY anchors.
#[must_use]
pub fn sexp_shape(s: &Sexp) -> SexpShape {
    s.shape()
}

/// Thin delegate to [`Sexp::type_name`] retained for callers that
/// want the free-function reach — the canonical site is now the
/// inherent method on the [`Sexp`] algebra. Stable, human-readable
/// name of a `Sexp`'s outermost shape — the `&'static str`
/// projection of `s.shape().label()`. Retained for callers that
/// want the canonical literal directly (e.g. test assertions on the
/// rendered `expected X, got Y` substring); new code constructing
/// `LispError::TypeMismatch` / `NamedFormNonSymbolName` passes
/// through `sexp_shape` directly so the typed identity rides the
/// variant slot rather than collapsing through the literal at the
/// helper boundary.
///
/// Composition law: `sexp_type_name(s) == s.type_name() ==
/// s.shape().label()` for every `s: &Sexp`. Pre-lift the dispatcher
/// lived here as the canonical site; post-lift the inherent method
/// [`Sexp::type_name`] is the canonical site and this free function
/// delegates so existing callers continue to compile. Same lift
/// posture as [`super::domain::sexp_shape`] → [`Sexp::shape`]
/// (commit 121bb60), [`super::domain::sexp_witness`] →
/// [`Sexp::witness`] (commit a427e3b), [`super::domain::sexp_to_json`]
/// → [`Sexp::to_json`] (commit 875ee3b), and
/// [`super::domain::json_to_sexp`] → [`Sexp::from_json`] (commit
/// 4a467eb): the algebra-level projection sits on the value, the
/// free function is a one-line thin delegate. The
/// `LispError::TypeMismatch.got` projection at
/// `compile::compile_typed`'s typed-entry rejection site and every
/// legacy substring-grep rejection-message test routes through
/// `s.type_name()` after this lift.
///
/// Sibling of [`sexp_shape`] (the typed-shape projection feeding
/// `TypeMismatch.expected` typed slot) and [`sexp_witness`] (the
/// joint typed-shape + renderable-literal projection feeding
/// `NamedFormNonSymbolName.got` / `NonSymbolUnquoteTarget.got` /
/// etc.). [`Sexp::type_name`] is the canonical-label-only
/// projection — the `&'static str` literal flattened from the
/// typed identity for substring-grep callers and the
/// `TypeMismatch.got` slot.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform / constructive
/// diagnostics. The canonical-label projection becomes a NAMED
/// primitive on the substrate's `Sexp` algebra rather than a free
/// function consumers reach across module boundaries to call.
/// THEORY.md §VI.1 — generation over composition; the projection now
/// lives on the typed `Sexp` algebra alongside `Sexp::shape` /
/// `Sexp::witness` / `Sexp::to_json` / `Sexp::from_json`, so a
/// future `Sexp` variant lands at the algebra's match site (via
/// `Sexp::shape`'s exhaustive arm) without a module-path
/// indirection. THEORY.md §II.1 invariant 1 — typed entry; the
/// offending Sexp's canonical-label identity is part of the proof
/// of WHAT the typed-entry gate rejected.
#[must_use]
pub fn sexp_type_name(s: &Sexp) -> &'static str {
    s.type_name()
}

/// Thin delegate to [`Sexp::witness`] retained for callers that want
/// the free-function reach — the canonical site is now the inherent
/// method on the [`Sexp`] algebra. Pairs the typed [`SexpShape`]
/// (structural identity) with the renderable [`Sexp::Display`]
/// projection in ONE owned [`SexpWitness`] value so the variant lives
/// independent of the call frame and crosses thread boundaries
/// cleanly.
///
/// Composition law: `sexp_witness(s) == s.witness()` for every
/// `s: &Sexp`. Pre-lift the dispatcher lived here as the canonical
/// site; post-lift the inherent method [`Sexp::witness`] is the
/// canonical site and this free function delegates so existing
/// callers continue to compile. Same lift posture as
/// [`super::domain::sexp_shape`] → [`Sexp::shape`] (commit 121bb60):
/// the algebra-level projection sits on the value, the free
/// function is a one-line thin delegate. The 8 typed-entry
/// rejection-builder callers in `macro_expand.rs`
/// (`non_symbol_unquote_target`, `splice_outside_list`,
/// `non_symbol_param`, `rest_param_missing_name`,
/// `rest_param_trailing_tokens`, `optional_param_malformed`,
/// `defmacro_non_symbol_name`, `defmacro_non_list_params`), the
/// `missing_head_err` invocation in the `TataraDomain` blanket impl
/// at line 46, and the typed-exit `rewriter_non_list_err` builder
/// all route through `s.witness()` after this lift.
///
/// Sibling of [`sexp_shape`] (the shape-only projection feeding
/// `TypeMismatch.got` / `NamedFormNonSymbolName.got`) and
/// [`sexp_type_name`] (the `&'static str`-only projection feeding
/// legacy substring-grep consumers). [`Sexp::witness`] is the
/// typed JOINT projection — both halves of the identity bundled
/// into ONE owned `SexpWitness` value.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform / constructive
/// diagnostics. An error that names only the shape leaves the operator
/// to guess what they wrote; an error that names only the literal
/// withholds the structural identity tools want to pattern-match on.
/// The witness names both. THEORY.md §VI.1 — generation over
/// composition; the projection now lives on the typed `Sexp` algebra
/// alongside `Sexp::shape`, so a future `Sexp` variant lands at the
/// algebra's match site (via `Sexp::shape`'s exhaustive arm) without
/// a module-path indirection. THEORY.md §II.1 invariant 1 — typed
/// entry; the offending Sexp's identity is part of the proof of WHAT
/// the typed-entry gate rejected.
#[must_use]
pub fn sexp_witness(s: &Sexp) -> SexpWitness {
    s.witness()
}
// ── Near-match suggestion ──────────────────────────────────────────
//
// The metric itself lives in `tatara-closed-set`, its only consumer
// (`ClosedSet::suggest_closest`). It was briefly homed here by phase 2
// step 1, which created a `tatara-closed-set → tatara-lisp` edge and so
// made the reverse edge — the one step 2 needs, to carry LispError
// variants whose payloads are ClosedSet implementors — a cargo cycle.
// INVERTed: `suggest` went back to its only call site and this crate
// depends on that one instead.
//
// Re-exported rather than moved-and-forgotten so `tatara_lisp::domain::suggest`
// still resolves — A-side parity, and step 3's `unknown_kwarg` /
// `suggest_keyword` hints reach the metric through this path.
// One primitive, one implementation of edit distance, either way.

/// The substrate's bounded edit-distance near-match metric.
///
/// Defined in [`tatara_closed_set`] (its only consumer) and re-exported
/// here so `tatara_lisp::domain::suggest` stays the canonical path.
pub use tatara_closed_set::suggest;

/// Structural duplicate-kwarg builder. Returns the dedicated
/// `LispError::DuplicateKwarg` variant so authoring surfaces (REPL, LSP,
/// `tatara-check`) bind to a first-class `key` field instead of
/// substring-parsing the rendered message. Display matches the legacy
/// `Compile { form: kwarg_form(key), message: "duplicate keyword" }`
/// rendering byte-for-byte (`"compile error in :{key}: duplicate
/// keyword"`), so existing `msg.contains("duplicate keyword")` /
/// `msg.contains(":name")` assertions keep passing.
///
/// Two inline copies of the same triple — `parse_kwargs`'s top-level
/// duplicate-keyword path and `sexp_to_json`'s nested-kwargs duplicate-
/// keyword path — used to assemble this shape by hand. One named
/// primitive lifts both into the substrate's structural-variant surface,
/// so every `parse_kwargs` failure mode (`OddKwargs` for odd length,
/// `TypeMismatch` for not-a-keyword-at-position, `DuplicateKwarg` for
/// duplicate key) is now a structural variant of `LispError`, not a
/// `Compile`-shaped substring.
///
/// Theory anchor: THEORY.md §V.1 — "Knowable platform … Render
/// Anywhere." A diagnostic whose offending `key` is embedded in a
/// free-form message is structurally incomplete; an authoring surface
/// that wants to render a squiggly under the duplicate or hint a fix
/// must re-parse the message. After this lift the slot exists in the
/// variant's data shape itself. THEORY.md §II.1 invariant 1 (typed
/// entry — "Ill-typed input errors before the value exists") — a
/// duplicate kwarg is exactly the failure mode the typed-entry gate
/// exists to reject; naming it structurally is the typed posture for
/// that gate's diagnostic.
#[must_use]
pub fn duplicate_kwarg(key: &str) -> LispError {
    LispError::DuplicateKwarg {
        key: key.to_string(),
    }
}

/// Structural missing-kwarg builder. Returns the dedicated
/// `LispError::MissingKwarg` variant so authoring surfaces (REPL, LSP,
/// `tatara-check`) bind to a first-class `key` field instead of
/// substring-parsing the rendered message. Display matches the legacy
/// `Compile { form: kwarg_form(key), message: "required but not
/// provided" }` rendering byte-for-byte (`"compile error in :{key}:
/// required but not provided"`), so existing
/// `msg.contains("required")` / `msg.contains(":threshold")` assertions
/// keep passing.
///
/// `required` (the kwarg lookup helper that fronts every typed
/// extractor — `extract_string`, `extract_int`, `extract_float`,
/// `extract_bool`, `extract_via_serde`, plus every hand-written
/// `TataraDomain` impl in the forge / lattice / tameshi crates) used
/// to assemble this shape inline. One named primitive lifts that into
/// the substrate's structural-variant surface, so every kwarg-level
/// "required-but-absent" failure routes through ONE function instead
/// of re-formatting the shape per call site. After this lift every
/// distinct `parse_kwargs` + `required` typed-entry kwarg failure mode
/// (odd length, not-a-keyword-at-position, duplicate key, missing
/// required key) is now a structural variant of `LispError`, not a
/// `Compile`-shaped substring.
///
/// Sibling of the pre-existing `Missing(&'static str)` variant —
/// `MissingKwarg` covers the runtime-key path the kwargs extractors
/// share (every derive-generated extractor and every hand-written
/// `TataraDomain` impl); `Missing` stays for compile-time-known names.
///
/// Theory anchor: THEORY.md §V.1 — "Knowable platform … Render
/// Anywhere." A diagnostic whose offending `key` is embedded in a
/// free-form message is structurally incomplete; an authoring surface
/// that wants to render a squiggly under the missing kwarg slot or
/// render a "did you mean :X?" hint must re-parse the message. After
/// this lift the slot exists in the variant's data shape itself.
/// THEORY.md §II.1 invariant 1 (typed entry — "Ill-typed input errors
/// before the value exists") — a missing required kwarg is exactly the
/// failure mode the typed-entry gate exists to reject; naming it
/// structurally is the typed posture for that gate's diagnostic.
#[must_use]
pub fn missing_kwarg(key: &str) -> LispError {
    LispError::MissingKwarg {
        key: key.to_string(),
    }
}

/// Structural type-mismatch builder. Pairs a typed `form: KwargPath`
/// (typically `kwarg_form(_)` / `kwarg_item_form(_, _)` /
/// `kwargs_pos_form(_)`) with the static `expected` label and the `got`
/// projection of the offending `Sexp` through `sexp_type_name`. Returns
/// the dedicated `LispError::TypeMismatch` variant so authoring surfaces
/// (REPL, LSP, `tatara-check`) bind to first-class `form`/`expected`/`got`
/// fields — pattern-matching on `KwargPath::Item { .. }` etc. directly —
/// instead of substring-parsing the rendered message.
///
/// Three inline `format!("expected {X}, got {}", sexp_type_name(_))`
/// copies in this module (`type_err`, `extract_string_list` per-item,
/// `extract_vec_via_serde` non-list) used to assemble the same shape by
/// hand; the three-times rule (THEORY.md §VI.1) calls for one named
/// primitive. This is it. Future runs that thread `pos: Option<usize>`
/// from `Sexp` spans add ONE field to the variant; every type-mismatch
/// site inherits positional rendering with no consumer changes.
#[must_use]
pub fn type_mismatch(
    form: crate::error::KwargPath,
    expected: ExpectedKwargShape,
    got: &Sexp,
) -> LispError {
    LispError::TypeMismatch {
        form,
        expected,
        got: got.shape(),
    }
}

fn type_err(key: &str, expected: ExpectedKwargShape, got: &Sexp) -> LispError {
    type_mismatch(kwarg_form(key), expected, got)
}

/// Item-indexed sibling of `type_err` — pairs `kwarg_item_form` with
/// `type_mismatch` so a per-item failure inside a list-typed kwarg names
/// `KwargPath::Item { key, idx }` plus the structural `expected`/`got` shape.
/// Used by `extract_string_list`'s per-item path; future per-item type-mismatch
/// sites (e.g. typed enums-of-strings, typed numeric vecs) bind here
/// rather than re-inlining the shape.
fn type_err_at(key: &str, idx: usize, expected: ExpectedKwargShape, got: &Sexp) -> LispError {
    type_mismatch(kwarg_item_form(key, idx), expected, got)
}

/// Range-axis sibling of [`type_err`] — the kwarg's `Sexp` shape was
/// RIGHT but the value does not fit the field's Rust width. Pairs the
/// same `kwarg_form(_)` typed path with the typed `NumericWidth` target
/// and the author's literal, returning [`LispError::KwargOutOfRange`].
///
/// Named beside `type_err` / `type_err_at` deliberately: the three are
/// the typed-entry kwarg gate's whole rejection vocabulary — shape,
/// per-item shape, and range — so a reader who finds one finds all
/// three, and a future span lift touches one neighbourhood.
fn range_err(key: &str, target: NumericWidth, value: NumericLiteral) -> LispError {
    LispError::KwargOutOfRange {
        form: kwarg_form(key),
        target,
        value,
    }
}

/// Required atomic-kwarg extractor — fronts every typed-atom public
/// `extract_X` helper (`extract_string`, `extract_int`, `extract_float`,
/// `extract_bool`). The four byte-identical inline shapes —
///
/// ```ignore
/// let v = required(kw, key)?;
/// v.as_X().ok_or_else(|| type_err(key, "<X-name>", v))
/// ```
///
/// — collapse to ONE generic primitive parameterized by the projection
/// function `project: FnOnce(&'a Sexp) -> Option<T>` and the typed-name
/// label `expected: &'static str`. The four-times rule (THEORY.md §VI.1)
/// is decisively crossed; lifting it into ONE primitive means the next
/// change to the typed-atom failure-projection shape (e.g. threading
/// `pos: Option<usize>` once `Sexp` carries spans, attaching a structural
/// `source: SexpTypeMismatch` chain) lands as ONE signature change inside
/// `extract_atom`, and all four public extractors pick up the upgrade
/// mechanically — no per-extractor edit, no per-extractor test drift.
///
/// `T` is generic so the helper handles both owned (`i64`, `f64`, `bool`)
/// and borrowed (`&'a str`) projections uniformly — the lifetime
/// threading `&'a Sexp → Option<&'a str>` works because every
/// `Sexp::as_*` method is `for<'b> fn(&'b Self) -> Option<…&'b str…>`;
/// the helper inherits that lifetime quantification through
/// `FnOnce(&'a Sexp) -> Option<T>`. Calling `extract_atom(kw, key,
/// "string", Sexp::as_string)` infers `T = &'a str`; calling
/// `extract_atom(kw, key, "int", Sexp::as_int)` infers `T = i64`.
///
/// Sibling of `extract_optional_atom` for the optional kwarg path —
/// together the two close every distinct typed-atom kwarg extractor's
/// shape: required vs. optional, returning `Result<T>` vs.
/// `Result<Option<T>>` from the same underlying projection. Future
/// extension to additional atomic types (e.g. `Atom::Bytes` if/when
/// added) is ONE one-line public delegate plus ONE call site — no
/// new error-path duplication.
///
/// Theory anchor: THEORY.md §VI.1 — generation over composition;
/// three-times rule decisively crossed (four byte-identical
/// extract+project+type-err shapes across `extract_string`,
/// `extract_int`, `extract_float`, `extract_bool`). THEORY.md §V.1 —
/// knowable platform / constructive diagnostics: the typed-atom
/// kwarg-failure projection lives in ONE primitive so authoring
/// surfaces (`tatara-check`, REPL, LSP) pick up the diagnostic-shape
/// promotion mechanically once the variant is structurally extended.
/// THEORY.md §II.1 invariant 1 — typed entry; the typed-atom
/// extractor IS the rust-level typed-entry gate for primitive kwargs,
/// and naming its single shape lifts the gate from four-site
/// duplication to one rust function the substrate's diagnostic
/// promotions hang off of.
fn extract_atom<'a, T, F>(
    kw: &'a Kwargs<'a>,
    key: &str,
    expected: ExpectedKwargShape,
    project: F,
) -> Result<T>
where
    F: FnOnce(&'a Sexp) -> Option<T>,
{
    let v = required(kw, key)?;
    project(v).ok_or_else(|| type_err(key, expected, v))
}

/// Optional sibling of `extract_atom` — collapses the four byte-identical
/// inline shapes of `extract_optional_string`, `extract_optional_int`,
/// `extract_optional_float`, `extract_optional_bool`:
///
/// ```ignore
/// match kw.get(key) {
///     None => Ok(None),
///     Some(v) => v.as_X().map(Some).ok_or_else(|| type_err(key, "<X-name>", v)),
/// }
/// ```
///
/// into ONE generic primitive. Same `T`/`project`/`expected` shape as
/// `extract_atom`; the difference is the `kw.get(key)` short-circuit at
/// the `None` arm — an absent kwarg is not an error for optional
/// extractors, only a malformed-present one is. The `.copied()` on
/// `kw.get(key)` projects `Option<&&'a Sexp>` to `Option<&'a Sexp>` so
/// the `project` call gets the same `&'a Sexp` shape as the required
/// path — type-checks against the same projection functions
/// (`Sexp::as_string`, `Sexp::as_int`, etc.) without per-call casts.
///
/// Future structural promotion of the type-mismatch diagnostic lands at
/// ONE call site inside this helper — same property as `extract_atom`.
fn extract_optional_atom<'a, T, F>(
    kw: &'a Kwargs<'a>,
    key: &str,
    expected: ExpectedKwargShape,
    project: F,
) -> Result<Option<T>>
where
    F: FnOnce(&'a Sexp) -> Option<T>,
{
    match optional(kw, key) {
        None => Ok(None),
        Some(v) => project(v)
            .map(Some)
            .ok_or_else(|| type_err(key, expected, v)),
    }
}

/// List-typed kwarg extractor — fronts every public `extract_*` helper
/// that reads a kwarg as a `Sexp::List` and projects each element to an
/// owned `T`. The two byte-identical inline skeletons —
///
/// ```ignore
/// let Some(v) = kw.get(key).copied() else { return Ok(Vec::new()) };
/// let list = v.as_list().ok_or_else(|| type_err(key, <list-shape>, v))?;
/// list.iter().enumerate().map(<per-item>).collect()
/// ```
///
/// — `extract_string_list` (each item projected via `as_string`, per-item
/// failure via `type_err_at`) and `extract_vec_via_serde` (each item via
/// `from_value_with_path`, per-item failure carrying `KwargPath::item`) —
/// collapse to ONE generic primitive parameterized by the outer-shape
/// label `list_shape: ExpectedKwargShape` and the per-element projection
/// `item: FnMut(usize, &Sexp) -> Result<T>`. The skeleton owns the three
/// fixed decisions both extractors share: absent kwarg → `Ok(Vec::new())`
/// (an absent list kwarg is the empty list, never an error — same posture
/// `extract_optional_atom` takes for absent atoms); present-but-not-a-list
/// → `type_err(key, list_shape, v)` (the outer-shape gate, labeled by the
/// caller-supplied `list_shape` so `ListOfStrings` vs. `List` stays a
/// per-caller decision, not baked into the skeleton); and the
/// `iter().enumerate().map(item).collect()` per-element walk that threads
/// the element index into the projection so per-item diagnostics can name
/// `:<key>[<idx>]` without re-counting from the source.
///
/// This is the list-family sibling of `extract_atom` / `extract_optional_atom`
/// (the atom-family generic projection primitives). Together the three close
/// every distinct typed-kwarg extractor's outer skeleton: required atom,
/// optional atom, and list. The per-element projection is `FnMut(usize,
/// &Sexp) -> Result<T>` — generic over `T` so it handles both the owned-
/// `String` (`extract_string_list`) and `DeserializeOwned`-`T`
/// (`extract_vec_via_serde`) element shapes uniformly, and threading the
/// `usize` index lets the projection construct the item-keyed
/// `KwargPath::Item { key, idx }` / `type_err_at` path the per-item gate
/// reports through.
///
/// Future structural promotion of the outer not-a-list diagnostic, or a
/// move to a fallible-streaming collect that short-circuits on the first
/// bad element with its position, lands at ONE site inside this helper —
/// both public list extractors pick up the upgrade mechanically, same
/// property `extract_atom` gives the four atom extractors.
///
/// Theory anchor: THEORY.md §VI.1 — generation over composition; the
/// list-typed extractor skeleton recurs at two sites (the PRIME-DIRECTIVE
/// ≥2 trigger) and is lifted to one owner, exactly as the atom skeleton was.
/// THEORY.md §V.1 — knowable platform; the list-kwarg outer gate + per-item
/// path live in ONE primitive so authoring surfaces (`tatara-check`, REPL,
/// LSP) pick up diagnostic-shape promotions once, not per-extractor.
/// THEORY.md §II.1 invariant 1 — typed entry; the list extractor IS the
/// rust-level typed-entry gate for list-shaped kwargs, and naming its single
/// skeleton lifts the gate from two-site duplication to one function the
/// substrate's diagnostic promotions hang off of.
fn extract_list<T, F>(
    kw: &Kwargs<'_>,
    key: &str,
    list_shape: ExpectedKwargShape,
    mut item: F,
) -> Result<Vec<T>>
where
    F: FnMut(usize, &Sexp) -> Result<T>,
{
    let Some(v) = optional(kw, key) else {
        return Ok(Vec::new());
    };
    let list = v.as_list().ok_or_else(|| type_err(key, list_shape, v))?;
    list.iter()
        .enumerate()
        .map(|(idx, e)| item(idx, e))
        .collect()
}

pub fn extract_string<'a>(kw: &'a Kwargs<'a>, key: &str) -> Result<&'a str> {
    extract_atom(kw, key, ExpectedKwargShape::String, Sexp::as_string)
}

pub fn extract_optional_string<'a>(kw: &'a Kwargs<'a>, key: &str) -> Result<Option<&'a str>> {
    extract_optional_atom(kw, key, ExpectedKwargShape::String, Sexp::as_string)
}

pub fn extract_string_list(kw: &Kwargs<'_>, key: &str) -> Result<Vec<String>> {
    extract_list(kw, key, ExpectedKwargShape::ListOfStrings, |idx, s| {
        s.as_string()
            .map(String::from)
            .ok_or_else(|| type_err_at(key, idx, ExpectedKwargShape::String, s))
    })
}

pub fn extract_int(kw: &Kwargs<'_>, key: &str) -> Result<i64> {
    extract_atom(kw, key, ExpectedKwargShape::Int, Sexp::as_int)
}

pub fn extract_optional_int(kw: &Kwargs<'_>, key: &str) -> Result<Option<i64>> {
    extract_optional_atom(kw, key, ExpectedKwargShape::Int, Sexp::as_int)
}

pub fn extract_float(kw: &Kwargs<'_>, key: &str) -> Result<f64> {
    extract_atom(kw, key, ExpectedKwargShape::Number, Sexp::as_float)
}

pub fn extract_optional_float(kw: &Kwargs<'_>, key: &str) -> Result<Option<f64>> {
    extract_optional_atom(kw, key, ExpectedKwargShape::Number, Sexp::as_float)
}

// ── The narrowing axis: a wide reader value → the field's Rust width ──
//
// `extract_int` returns `i64` and `extract_float` returns `f64` — the
// widest thing the reader can hand back on each axis. A field declared
// `u32` / `i32` / `usize` / `f32` is NARROWER, and the projection from
// the reader's width to the field's is a partial function: `70000` has
// no `u16`, `-1` has no `u32`, `1e300` has no `f32`.
//
// `#[derive(TataraDomain)]` used to close that gap with a raw Rust `as`
// cast appended to the extractor call — `extract_int(&kw, "port")? as
// u32`. `as` is TOTAL by truncating: it wraps, sign-flips, or saturates
// to `inf` and reports nothing. So an author who wrote a number the
// field could not hold got a DIFFERENT number in the struct, silently,
// with a green build and a green parse. That is precisely the class the
// typed-entry gate exists to reject, leaking through the one hole the
// gate did not cover.
//
// The fix is one typed primitive, here, rather than a check at each of
// the derive's four numeric arms — same lift as the `extract_via_serde`
// family below, and for the same reason: a hand-written `TataraDomain`
// impl and a derived one must take the identical error path, and the
// next upgrade (a span, a suggested-value hint) has to land in ONE
// place.
//
// The target width rides the TYPE, not an argument. `NarrowNumeric`'s
// associated `WIDTH` const means the derive emits
// `extract_int_narrowed::<u32>(&kw, key)?` and cannot mislabel the
// diagnostic, because it never names the width at all — the impl does.
// A future `classify` arm for a width with no impl is a compile error
// at the consumer, not a mislabeled runtime message.

/// A narrower numeric type reachable from the reader's wide `Wide`
/// value — `i64` on the int axis, `f64` on the float axis.
///
/// Implemented for exactly the seven widths `#[derive(TataraDomain)]`
/// recognises, which is what makes [`NumericWidth`] a genuinely closed
/// set: the enum's variants and this trait's impls are the same list,
/// generated from the same macro invocation below.
pub trait NarrowNumeric<Wide>: Sized + Copy {
    /// This type's identity in the typed diagnostic — the value that
    /// rides [`LispError::KwargOutOfRange`]'s `target` slot.
    const WIDTH: NumericWidth;

    /// The partial projection. `None` means "this wide value has no
    /// representation at this width" and becomes a typed rejection;
    /// it never means "here is a nearby value instead".
    fn narrow(wide: Wide) -> Option<Self>;
}

/// Emit the `NarrowNumeric<i64>` impls for the integer widths. Each is
/// `TryFrom<i64>` verbatim — the std conversion is already the exact
/// partial function we want (rejects too-large AND negative-into-
/// unsigned), so the impl delegates rather than re-deriving bounds
/// arithmetic that could disagree with std.
macro_rules! impl_narrow_int {
    ($($ty:ty => $width:ident),+ $(,)?) => {$(
        impl NarrowNumeric<i64> for $ty {
            const WIDTH: NumericWidth = NumericWidth::$width;
            fn narrow(wide: i64) -> Option<Self> {
                <$ty as ::core::convert::TryFrom<i64>>::try_from(wide).ok()
            }
        }
    )+};
}

impl_narrow_int! {
    i32 => I32,
    i64 => I64,
    u32 => U32,
    u64 => U64,
    usize => Usize,
}

impl NarrowNumeric<f64> for f64 {
    const WIDTH: NumericWidth = NumericWidth::F64;
    fn narrow(wide: f64) -> Option<Self> {
        Some(wide)
    }
}

impl NarrowNumeric<f64> for f32 {
    const WIDTH: NumericWidth = NumericWidth::F32;
    /// Rejects exactly one thing: a FINITE `f64` whose magnitude
    /// overflows to `inf` at `f32`. Precision loss inside the range is
    /// accepted — an `f32` field asked for `f32` precision, and
    /// rejecting `0.1` would make the type unusable. An input that was
    /// already `inf` / `NaN` passes through unchanged, because `as`
    /// preserved it faithfully; the corruption case is only the finite
    /// value that becomes infinite.
    fn narrow(wide: f64) -> Option<Self> {
        #[allow(clippy::cast_possible_truncation)]
        let narrowed = wide as f32;
        if narrowed.is_finite() || !wide.is_finite() {
            Some(narrowed)
        } else {
            None
        }
    }
}

/// Required integer kwarg projected into the field's own width —
/// [`extract_int`] followed by the typed [`NarrowNumeric`] projection,
/// with [`LispError::KwargOutOfRange`] on a value the width cannot
/// hold. The narrowing replacement for `extract_int(&kw, key)? as T`.
pub fn extract_int_narrowed<T: NarrowNumeric<i64>>(kw: &Kwargs<'_>, key: &str) -> Result<T> {
    let wide = extract_int(kw, key)?;
    T::narrow(wide).ok_or_else(|| range_err(key, T::WIDTH, NumericLiteral::Int(wide)))
}

/// `Option` sibling of [`extract_int_narrowed`]. An ABSENT kwarg stays
/// `None`; a PRESENT but out-of-range one is a rejection, never a
/// `None` — silently dropping a value the author wrote would be the
/// same corruption in a different costume.
pub fn extract_optional_int_narrowed<T: NarrowNumeric<i64>>(
    kw: &Kwargs<'_>,
    key: &str,
) -> Result<Option<T>> {
    let Some(wide) = extract_optional_int(kw, key)? else {
        return Ok(None);
    };
    T::narrow(wide)
        .map(Some)
        .ok_or_else(|| range_err(key, T::WIDTH, NumericLiteral::Int(wide)))
}

/// Float-axis sibling of [`extract_int_narrowed`] — [`extract_float`]
/// followed by the typed [`NarrowNumeric`] projection. The narrowing
/// replacement for `extract_float(&kw, key)? as T`.
pub fn extract_float_narrowed<T: NarrowNumeric<f64>>(kw: &Kwargs<'_>, key: &str) -> Result<T> {
    let wide = extract_float(kw, key)?;
    T::narrow(wide).ok_or_else(|| range_err(key, T::WIDTH, NumericLiteral::Float(wide)))
}

/// `Option` sibling of [`extract_float_narrowed`]; same absent-vs-
/// out-of-range split as [`extract_optional_int_narrowed`].
pub fn extract_optional_float_narrowed<T: NarrowNumeric<f64>>(
    kw: &Kwargs<'_>,
    key: &str,
) -> Result<Option<T>> {
    let Some(wide) = extract_optional_float(kw, key)? else {
        return Ok(None);
    };
    T::narrow(wide)
        .map(Some)
        .ok_or_else(|| range_err(key, T::WIDTH, NumericLiteral::Float(wide)))
}

pub fn extract_bool(kw: &Kwargs<'_>, key: &str) -> Result<bool> {
    extract_atom(kw, key, ExpectedKwargShape::Bool, Sexp::as_bool)
}

pub fn extract_optional_bool(kw: &Kwargs<'_>, key: &str) -> Result<Option<bool>> {
    extract_optional_atom(kw, key, ExpectedKwargShape::Bool, Sexp::as_bool)
}

// ── Universal serde-Deserialize fallthrough (enums, nested structs, …) ──
//
// `#[derive(TataraDomain)]` covers `String` / numeric / `bool` / their
// `Option` and `Vec<String>` shapes with the typed extractors above. Any
// field type outside that closed set falls through to these helpers, which
// project the kwarg `Sexp` to canonical JSON via `sexp_to_json` and feed
// it to `serde_json::from_value` — works for any `serde::Deserialize`.
//
// The shape used to live inline in three `quote!` blocks in the derive
// macro (`Kind::Deserialize`, `Kind::OptionalDeserialize`,
// `Kind::VecDeserialize`). Lifting them here means:
//   - Hand-written `TataraDomain` impls share the same error path.
//   - Future diagnostic upgrades (attaching a source position once `Sexp`
//     carries spans, richer field-path traces) happen in ONE function,
//     not three macro-emitted copies.
//   - The `:<key> deserialize: …` message is a single named primitive in
//     the substrate — `tatara-check` / LSP / REPL render it uniformly.
//
// Both helpers below funnel through the structural
// `LispError::KwargDeserialize { path: KwargPath, message }` variant —
// the typed-entry-side `from_value` mirror of the typed-exit-side
// `to_value` `LispError::DomainSerialize { keyword, message }` lift. The
// two sites bifurcate via the typed `KwargPath` enum's variant identity:
// `KwargPath::Named(key)` for kwarg-keyed failures (the
// `extract_via_serde` / `extract_optional_via_serde` path),
// `KwargPath::Item { key, idx }` for kwarg-AND-index-keyed failures (the
// `extract_vec_via_serde` per-item path). After this lift the
// `from_value` boundary's two distinct rejection modes BOTH bind to ONE
// structural variant of `LispError`, not a `Compile`-shaped substring;
// the `(key, idx: Option<usize>)` bifurcation collapses into
// `KwargPath`'s `Named` vs. `Item` variant identity, so the invalid
// sibling-slot combination `(key: "", idx: Some(_))` for a scalar path
// is structurally unrepresentable rather than re-asserted at the helper
// boundary via runtime `Option::is_some` comparison. Together with
// `DomainSerialize`, every distinct `serde_json` failure mode at the
// typed-domain JSON boundary — both directions of the round-trip — is
// now structurally typed. This is the LAST `LispError::Compile { ... }`
// construction site in this file.
//
// Theory anchor: THEORY.md §VI.1 (generation over composition — the
// generator must lean on the library, not duplicate the library inline).
// THEORY.md §II.1 invariant 1 (typed entry) — `from_value` failures are
// exactly the failure mode the typed-entry JSON gate exists to reject;
// naming them structurally is the typed posture for that gate's
// diagnostic.

/// Project a single `&Sexp` through the typed-entry JSON boundary —
/// `sexp_to_json` canonical-JSON projection + `serde_json::from_value::<T>`
/// + structural `LispError::KwargDeserialize { path, message }` on failure.
///
/// THREE call sites in this module used to assemble this shape inline:
/// `extract_via_serde` (required scalar kwarg path), `extract_optional_via_serde`
/// (optional scalar kwarg path), and `extract_vec_via_serde`'s per-item
/// closure (each item in a `Vec<T>` kwarg). The three byte-identical
/// `let json = sexp_to_json(sexp)?; serde_json::from_value(json).map_err(|e|
/// deserialize_*_err(<path-args>, &e))` shapes — modulo the typed
/// `KwargPath` constructor (`KwargPath::Named` vs. `KwargPath::Item`) —
/// collapse to ONE primitive parameterized by `path: KwargPath`. The
/// path's variant identity bifurcates scalar-vs-item rendering inside
/// `KwargPath`'s Display impl (`:<key>` vs. `:<key>[<idx>]`) so the helper
/// is shape-of-typed-entry-JSON-boundary, not shape-of-call-site.
///
/// After this lift the three-times-rule on the `from_value` projection
/// shape is decisively crossed; the two prior-run thin `deserialize_err`
/// / `deserialize_item_err` shims — which encapsulated only the
/// `KwargPath::named(_)` / `KwargPath::item(_,_)` constructor projection
/// over an already-extant `serde_json::Error` reference — are subsumed
/// by this primitive's `map_err` closure. The three extractor entry
/// points now bind on `from_value_with_path::<T>` directly with their
/// `KwargPath` constructed at the call boundary; the JSON-boundary's
/// rejection shape (`LispError::KwargDeserialize { path, message }`)
/// lives in ONE place — the `map_err` arm here — instead of being
/// re-asserted at three site-specific shims.
///
/// `<T: DeserializeOwned>` is generic so the helper handles every serde-
/// projectable typed-domain field uniformly — scalar `i64` / `String` /
/// nested struct / `Vec<Nested>` / enum-by-symbol — same posture as the
/// `extract_atom` / `extract_optional_atom` generic-projection primitives
/// for the atom-typed kwarg path. `path: KwargPath` flows into the
/// variant's typed slot directly (owned), parallel to how `type_mismatch`
/// threads `KwargPath` into `LispError::TypeMismatch.form`. A future
/// fourth path shape (e.g. `:<key>.<field>` for nested-struct kwarg
/// failures) extends `KwargPath` ONCE and rustc-enforces matching at
/// every projection site; this helper picks up the new shape mechanically
/// with no signature change.
///
/// Theory anchor: THEORY.md §VI.1 — generation over composition; the
/// three-times rule's load-bearing trigger. THEORY.md §V.1 — knowable
/// platform; the typed-entry JSON-projection boundary's rejection shape
/// lives in ONE primitive so authoring surfaces (`tatara-check`, REPL,
/// LSP) pick up the diagnostic-shape promotion mechanically once the
/// variant is structurally extended. THEORY.md §II.1 invariant 1 (typed
/// entry) — a `from_value` failure is exactly the failure mode the
/// typed-entry JSON gate exists to reject; naming its single shape lifts
/// the gate from three-site duplication to one rust function the
/// substrate's diagnostic promotions hang off of.
fn from_value_with_path<T: DeserializeOwned>(sexp: &Sexp, path: KwargPath) -> Result<T> {
    let json = sexp.to_json()?;
    serde_json::from_value(json).map_err(|e| LispError::KwargDeserialize {
        path,
        message: e.to_string(),
    })
}

/// Required field — feeds the kwarg's canonical-JSON projection to
/// `serde_json::from_value::<T>` via `from_value_with_path` with a
/// `KwargPath::Named(key)` path slot. Errors carry `:key` so authoring
/// tools can point at the offending kwarg.
pub fn extract_via_serde<T: DeserializeOwned>(kw: &Kwargs<'_>, key: &str) -> Result<T> {
    from_value_with_path(required(kw, key)?, KwargPath::named(key))
}

/// Optional field — `None` if the kwarg is absent; `Some(T)` after a
/// successful `from_value_with_path` round-trip with a `KwargPath::Named(key)`
/// path slot.
pub fn extract_optional_via_serde<T: DeserializeOwned>(
    kw: &Kwargs<'_>,
    key: &str,
) -> Result<Option<T>> {
    let Some(sexp) = optional(kw, key) else {
        return Ok(None);
    };
    from_value_with_path(sexp, KwargPath::named(key)).map(Some)
}

/// `Vec<T>` field — empty vec if the kwarg is absent; otherwise the kwarg
/// must be a `Sexp::List` and each item flows through `from_value_with_path`
/// with a `KwargPath::Item { key, idx }` path slot, naming both the outer
/// kwarg AND the failing item index in any per-item rejection.
pub fn extract_vec_via_serde<T: DeserializeOwned>(kw: &Kwargs<'_>, key: &str) -> Result<Vec<T>> {
    extract_list(kw, key, ExpectedKwargShape::List, |idx, item| {
        from_value_with_path(item, KwargPath::item(key, idx))
    })
}

// ── Domain registry (runtime-registered, callable by keyword) ───────

/// Erased handler that knows how to compile a form and hand back a typed
/// serde-JSON representation. JSON is the least-common-denominator typed
/// surface — every `TataraDomain` derives `serde::Serialize` by convention.
pub struct DomainHandler {
    pub keyword: &'static str,
    /// `type_name` of the Rust type that holds this keyword. Carried so a
    /// collision can NAME the incumbent, and so [`registrations`] can render a
    /// census an operator can read. Without it, "who already owns
    /// `defplugin`?" is answerable only by reading every crate that links in.
    pub owner: &'static str,
    pub compile: fn(args: &[Sexp]) -> Result<serde_json::Value>,
}

/// A second, DIFFERENT type tried to claim a keyword that is already held.
///
/// This is the typed form of a defect that used to be a discarded `insert`
/// return value. `register` writes into a process-global map; before this type
/// existed, a colliding registration silently displaced the incumbent and every
/// subsequent `lookup` compiled the wrong struct — no error, no log, no
/// diagnostic, and the winner decided by link and call order rather than by
/// anything an author wrote.
///
/// Not a [`LispError`] variant on purpose: nothing here is a source-level
/// mistake in a `.tlisp` file. It is a fact about how one *process* was
/// assembled, and a caller that hits it must fix its crate graph, not its Lisp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeywordCollision {
    /// The contested keyword, e.g. `"defplugin"`.
    pub keyword: &'static str,
    /// `type_name` of the type that got there first and KEPT the keyword.
    pub incumbent: &'static str,
    /// `type_name` of the type that was turned away.
    pub challenger: &'static str,
}

impl std::fmt::Display for KeywordCollision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "keyword `{}` is already registered to `{}`; `{}` was refused \
             (one keyword, one type, per process)",
            self.keyword, self.incumbent, self.challenger
        )
    }
}

impl std::error::Error for KeywordCollision {}

static REGISTRY: OnceLock<Mutex<HashMap<&'static str, DomainHandler>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<&'static str, DomainHandler>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `TataraDomain` type with the global dispatcher.
///
/// **First writer wins, and the loser is told.** Three outcomes, exhaustively:
///
/// | registry state | result |
/// |---|---|
/// | keyword free | inserted, `Ok(())` |
/// | keyword held by `T` itself | no-op, `Ok(())` — the documented idempotency |
/// | keyword held by a different type | registry UNCHANGED, `Err(KeywordCollision)` |
///
/// The middle row is why this is not simply "reject a second insert": several
/// crates call `Foo::register()` from more than one entry point (a `register_all`
/// seed plus a lazy path), and that has always been legitimate. Only the third
/// row is the defect, and it used to be spelled `insert` — displacing the
/// incumbent and discarding it.
///
/// `#[must_use]`: a discarded result is now a compiler warning at every call
/// site, which is the point. It is deliberately a warning and not a hard break
/// — every one of the call sites measured across the pleme-io tree on
/// 2026-07-31 (165 `domain::register::<…>` sites, 0 of them in tail position)
/// is a statement ending in `;`, so widening the return type from `()` to
/// `Result` compiles unchanged everywhere and merely gets loud.
///
/// **Tier-honest.** This makes a collision *impossible to be silent within one
/// process*: the value exists, it is typed, and it is `#[must_use]`. It does
/// NOT make a collision unrepresentable — a caller may still write `let _ =
/// register::<T>();`, and two crates in two repos can still declare the same
/// `#[tatara(keyword = "…")]` and never be linked together, so nothing here
/// observes them. Catching *that* needs a source-level census across the tree,
/// which is what `tatara-keywords` does; this function covers only what one
/// running process can see.
#[must_use = "a refused registration means this keyword is already held by another type; \
              ignoring the result restores the silent-overwrite defect"]
pub fn register<T>() -> std::result::Result<(), KeywordCollision>
where
    T: TataraDomain + serde::Serialize,
{
    let owner = std::any::type_name::<T>();
    let mut reg = registry().lock().unwrap();

    if let Some(existing) = reg.get(T::KEYWORD) {
        // Same type re-registering: the documented idempotency, kept.
        if existing.owner == owner {
            return Ok(());
        }
        // Different type: refuse, and leave the incumbent exactly where it is.
        // Reporting a rejection while still mutating would be strictly worse
        // than the overwrite it replaces.
        return Err(KeywordCollision {
            keyword: T::KEYWORD,
            incumbent: existing.owner,
            challenger: owner,
        });
    }

    reg.insert(
        T::KEYWORD,
        DomainHandler {
            keyword: T::KEYWORD,
            owner,
            compile: |args| {
                let v = T::compile_from_args(args)?;
                serde_json::to_value(&v).map_err(|e| LispError::Compile {
                    form: T::KEYWORD.to_string(),
                    message: format!("serialize: {e}"),
                })
            },
        },
    );
    Ok(())
}

/// Look up a handler by keyword.
pub fn lookup(keyword: &str) -> Option<DomainHandler> {
    let reg = registry().lock().unwrap();
    reg.get(keyword).map(|h| DomainHandler {
        keyword: h.keyword,
        owner: h.owner,
        compile: h.compile,
    })
}

/// List currently registered keywords.
pub fn registered_keywords() -> Vec<&'static str> {
    registry().lock().unwrap().keys().copied().collect()
}

/// The live census: every registered keyword paired with the `type_name` of the
/// type holding it, sorted by keyword.
///
/// The queryable form of "who owns what" inside a running process. A binary that
/// links two crates claiming one keyword can print this and see exactly one row
/// for the contested keyword, naming the winner — where before, the losing
/// handler had been dropped with nothing recording that it ever existed.
#[must_use]
pub fn registrations() -> Vec<(&'static str, &'static str)> {
    let reg = registry().lock().unwrap();
    let mut rows: Vec<(&'static str, &'static str)> =
        reg.values().map(|h| (h.keyword, h.owner)).collect();
    rows.sort_unstable();
    rows
}

// ── Capability registries — compounding metadata layer ────────────
//
// Each registered domain can ALSO carry capability metadata —
// orthogonal concerns the rest of the platform needs to ask about
// the type without importing it. Today: `RenderMetadata` (used by
// tatara-render to emit Kubernetes CR YAML without a hard-coded
// match). Future: `ComplianceMetadata`, `DocumentationMetadata`,
// `AttestationMetadata` — same shape, additional concerns.
//
// Each metadata kind has its own static registry parallel to
// `REGISTRY` (the handler registry). Domain crates call
// `register_render::<T>()` alongside `register::<T>()` during
// boot; consumers like `tatara-render` look up by keyword.

/// Type that knows its Kubernetes-CR rendering metadata. Tiny —
/// just constants. Implementing crates derive nothing; they
/// `impl RenderableDomain for FooSpec { … }` with three lines.
pub trait RenderableDomain {
    /// Kubernetes apiVersion the resource lives under
    /// (`gateway.networking.k8s.io/v1`, `cilium.io/v2`, etc.).
    const API_VERSION: &'static str;
    /// Kubernetes kind (`Gateway`, `CiliumNetworkPolicy`).
    const KIND: &'static str;
    /// Field name (in the typed JSON) that supplies the CR's
    /// `metadata.name`. Most domains use `name`; gateway-api
    /// uses `gateway_class_name`. Defaults via `Default` impl.
    const NAME_FIELD: &'static str = "name";
}

/// Erased render metadata — what `tatara-render` consumes.
#[derive(Clone, Copy, Debug)]
pub struct RenderHandler {
    pub keyword: &'static str,
    pub api_version: &'static str,
    pub kind: &'static str,
    pub name_field: &'static str,
}

static RENDER_REGISTRY: OnceLock<Mutex<HashMap<&'static str, RenderHandler>>> = OnceLock::new();

fn render_registry() -> &'static Mutex<HashMap<&'static str, RenderHandler>> {
    RENDER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `RenderableDomain`'s metadata. Idempotent.
/// Domain crates call this once at boot, alongside `register::<T>()`.
pub fn register_render<T>()
where
    T: TataraDomain + RenderableDomain,
{
    let handler = RenderHandler {
        keyword: T::KEYWORD,
        api_version: T::API_VERSION,
        kind: T::KIND,
        name_field: T::NAME_FIELD,
    };
    render_registry()
        .lock()
        .unwrap()
        .insert(T::KEYWORD, handler);
}

/// Look up render metadata by keyword.
#[must_use]
pub fn lookup_render(keyword: &str) -> Option<RenderHandler> {
    render_registry().lock().unwrap().get(keyword).copied()
}

/// List every keyword that has render metadata registered.
#[must_use]
pub fn registered_render_keywords() -> Vec<&'static str> {
    render_registry().lock().unwrap().keys().copied().collect()
}

// ── Documented capability ─────────────────────────────────────────
//
// Third capability layer (compile / render / doc). Each domain
// can carry its struct-level + field-level documentation strings
// for catalog browsers, IDE hover-help, and the `tatara doc`
// CLI to consult uniformly.

/// Type that knows its human-readable documentation. Tiny: one
/// `&'static str` for the type-level summary, plus an array of
/// (field, doc) pairs.
pub trait DocumentedDomain {
    /// Top-level docstring for the type — what an embedder sees
    /// when hovering the keyword in a catalog browser.
    const DOCSTRING: &'static str;
    /// Per-field docstrings, in declaration order. Empty when no
    /// docs were captured upstream (typical for hand-written
    /// domains until they fill them in). Forge-generated domains
    /// populate this from CRD `description` fields.
    const FIELD_DOCS: &'static [(&'static str, &'static str)];
}

/// Erased doc handle.
#[derive(Clone, Copy, Debug)]
pub struct DocHandler {
    pub keyword: &'static str,
    pub docstring: &'static str,
    pub field_docs: &'static [(&'static str, &'static str)],
}

static DOC_REGISTRY: OnceLock<Mutex<HashMap<&'static str, DocHandler>>> = OnceLock::new();

fn doc_registry() -> &'static Mutex<HashMap<&'static str, DocHandler>> {
    DOC_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `DocumentedDomain`'s metadata. Idempotent.
pub fn register_doc<T>()
where
    T: TataraDomain + DocumentedDomain,
{
    let handler = DocHandler {
        keyword: T::KEYWORD,
        docstring: T::DOCSTRING,
        field_docs: T::FIELD_DOCS,
    };
    doc_registry().lock().unwrap().insert(T::KEYWORD, handler);
}

/// Look up doc metadata by keyword.
#[must_use]
pub fn lookup_doc(keyword: &str) -> Option<DocHandler> {
    doc_registry().lock().unwrap().get(keyword).copied()
}

/// List every keyword that has doc metadata registered.
#[must_use]
pub fn registered_doc_keywords() -> Vec<&'static str> {
    doc_registry().lock().unwrap().keys().copied().collect()
}

// ── Dependent capability ──────────────────────────────────────────
//
// Fourth capability layer (compile / render / doc / deps). Each
// domain can declare which OTHER keywords its instances logically
// depend on. The rollout pipeline consumes this to topo-sort the
// `Plan` so deploys land in the right order — apply
// `defservice` before `defpodmonitor` before `defciliumnetworkpolicy`,
// drain in reverse.

/// Type-level dependency declarations. The strings are keywords
/// of OTHER domains this one expects to be present (e.g. a
/// `defciliumnetworkpolicy` depends on a `defservice` whose pods
/// it selects). The dependency relation is type-to-type, not
/// instance-to-instance — finer-grained refs live on the typed
/// resource value itself.
pub trait DependentDomain {
    /// Keywords this domain logically depends on. Empty by
    /// default for forge-generated domains since CRDs don't
    /// generally declare deps; hand-written domains override
    /// to capture real ordering constraints.
    const DEPENDS_ON: &'static [&'static str];
}

/// Erased dep handle — what the topo-sort consumer reads.
#[derive(Clone, Copy, Debug)]
pub struct DepsHandler {
    pub keyword: &'static str,
    pub depends_on: &'static [&'static str],
}

static DEPS_REGISTRY: OnceLock<Mutex<HashMap<&'static str, DepsHandler>>> = OnceLock::new();

fn deps_registry() -> &'static Mutex<HashMap<&'static str, DepsHandler>> {
    DEPS_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `DependentDomain`'s deps. Idempotent.
pub fn register_deps<T>()
where
    T: TataraDomain + DependentDomain,
{
    let handler = DepsHandler {
        keyword: T::KEYWORD,
        depends_on: T::DEPENDS_ON,
    };
    deps_registry().lock().unwrap().insert(T::KEYWORD, handler);
}

/// Look up dep metadata by keyword.
#[must_use]
pub fn lookup_deps(keyword: &str) -> Option<DepsHandler> {
    deps_registry().lock().unwrap().get(keyword).copied()
}

/// List every keyword that has dep metadata registered.
#[must_use]
pub fn registered_deps_keywords() -> Vec<&'static str> {
    deps_registry().lock().unwrap().keys().copied().collect()
}

// ── Schematic capability ──────────────────────────────────────────
//
// Fifth capability layer: per-domain JSON Schema export. Forge-
// generated domains preserve the source CRD's openAPIV3Schema
// verbatim; hand-written domains can either skip the layer or
// hand-curate a schema. Consumers: IDE hover-help, web
// validators, openapi exporters, admin-UI form generators —
// everyone who wants the typed shape without depending on the
// Rust struct directly.

pub trait SchematicDomain {
    /// JSON Schema source for this type. Preserved verbatim from
    /// the CRD's openAPIV3Schema for forge-generated domains;
    /// hand-curated for non-CRD domains. Consumers parse this on
    /// demand — keeping it as a static string avoids paying
    /// serde_json::Value at startup for every domain.
    const SCHEMA_JSON: &'static str;
}

#[derive(Clone, Copy, Debug)]
pub struct SchemaHandler {
    pub keyword: &'static str,
    pub schema_json: &'static str,
}

static SCHEMA_REGISTRY: OnceLock<Mutex<HashMap<&'static str, SchemaHandler>>> = OnceLock::new();

fn schema_registry() -> &'static Mutex<HashMap<&'static str, SchemaHandler>> {
    SCHEMA_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_schema<T>()
where
    T: TataraDomain + SchematicDomain,
{
    let handler = SchemaHandler {
        keyword: T::KEYWORD,
        schema_json: T::SCHEMA_JSON,
    };
    schema_registry()
        .lock()
        .unwrap()
        .insert(T::KEYWORD, handler);
}

#[must_use]
pub fn lookup_schema(keyword: &str) -> Option<SchemaHandler> {
    schema_registry().lock().unwrap().get(keyword).copied()
}

#[must_use]
pub fn registered_schema_keywords() -> Vec<&'static str> {
    schema_registry().lock().unwrap().keys().copied().collect()
}

// ── Attestable capability ─────────────────────────────────────────
//
// Sixth capability layer: each domain declares its **attestation
// namespace** — the bucket the tameshi BLAKE3 chain groups its
// resources under. The canonical hash itself is namespace-aware
// (`blake3(namespace || canonical_json(value))`) so two resources
// with identical content but different domains never collide in
// the attestation tree. Closes the trust loop in the rollout
// pipeline.

pub trait AttestableDomain {
    /// Bucket name for the tameshi attestation chain. Forge-
    /// generated CRD domains use the CRD's group (e.g.
    /// `gateway.networking.k8s.io`); hand-written domains pick
    /// a stable namespace (e.g. `pleme.io/ebpf`). The namespace
    /// is hashed into the resource's BLAKE3 so cross-domain
    /// collisions are impossible.
    const ATTESTATION_NAMESPACE: &'static str;
}

#[derive(Clone, Copy, Debug)]
pub struct AttestHandler {
    pub keyword: &'static str,
    pub namespace: &'static str,
}

static ATTEST_REGISTRY: OnceLock<Mutex<HashMap<&'static str, AttestHandler>>> = OnceLock::new();

fn attest_registry() -> &'static Mutex<HashMap<&'static str, AttestHandler>> {
    ATTEST_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_attest<T>()
where
    T: TataraDomain + AttestableDomain,
{
    let handler = AttestHandler {
        keyword: T::KEYWORD,
        namespace: T::ATTESTATION_NAMESPACE,
    };
    attest_registry()
        .lock()
        .unwrap()
        .insert(T::KEYWORD, handler);
}

#[must_use]
pub fn lookup_attest(keyword: &str) -> Option<AttestHandler> {
    attest_registry().lock().unwrap().get(keyword).copied()
}

#[must_use]
pub fn registered_attest_keywords() -> Vec<&'static str> {
    attest_registry().lock().unwrap().keys().copied().collect()
}

/// Compute a namespaced BLAKE3 attestation for a typed value.
///
/// `BLAKE3(ATTESTATION_NAMESPACE || ":" || canonical_json(value))`
///
/// The namespace prefix prevents cross-domain hash collisions in
/// the tameshi attestation tree — two resources with identical
/// JSON but different domain semantics produce different hashes.
/// The canonical-JSON serialization is what `serde_json::to_string`
/// produces; consumers can rely on the hash being stable across
/// processes given the same input value.
#[must_use]
pub fn attest_value(namespace: &str, value: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(value).unwrap_or_default();
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace.as_bytes());
    hasher.update(b":");
    hasher.update(canonical.as_bytes());
    hasher.finalize().to_hex().to_string()
}

// ── Validated capability ──────────────────────────────────────────
//
// Seventh capability layer: per-domain semantic validators. The
// first capability with **executable behavior** (not just static
// metadata) — the registry stores function pointers, not
// constants. Each domain plugs in its own logic; the env-level
// validator dispatches.

/// Type that carries a semantic validator for its typed values.
/// Default impl returns `Ok(())` — so domains opt in, never
/// out. The validator runs AFTER `compile_from_args` succeeds —
/// it's a chance to enforce cross-field invariants the type
/// system alone can't catch (e.g. "if `kind = :xdp`, `attach`
/// must include an interface").
pub trait ValidatedDomain {
    /// Validate the typed JSON form of a domain instance. The
    /// default returns Ok — domains override to add real checks.
    /// Errors carry a human-readable message naming the
    /// offending field + constraint.
    fn validate_value(_value: &serde_json::Value) -> std::result::Result<(), String> {
        Ok(())
    }
}

/// Erased validator handle — function pointer, no captured state.
#[derive(Clone, Copy)]
pub struct ValidateHandler {
    pub keyword: &'static str,
    pub validate: fn(&serde_json::Value) -> std::result::Result<(), String>,
}

impl std::fmt::Debug for ValidateHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidateHandler")
            .field("keyword", &self.keyword)
            .field("validate", &"<fn>")
            .finish()
    }
}

static VALIDATE_REGISTRY: OnceLock<Mutex<HashMap<&'static str, ValidateHandler>>> = OnceLock::new();

fn validate_registry() -> &'static Mutex<HashMap<&'static str, ValidateHandler>> {
    VALIDATE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_validate<T>()
where
    T: TataraDomain + ValidatedDomain,
{
    let handler = ValidateHandler {
        keyword: T::KEYWORD,
        validate: <T as ValidatedDomain>::validate_value,
    };
    validate_registry()
        .lock()
        .unwrap()
        .insert(T::KEYWORD, handler);
}

#[must_use]
pub fn lookup_validate(keyword: &str) -> Option<ValidateHandler> {
    validate_registry().lock().unwrap().get(keyword).copied()
}

#[must_use]
pub fn registered_validate_keywords() -> Vec<&'static str> {
    validate_registry()
        .lock()
        .unwrap()
        .keys()
        .copied()
        .collect()
}

// ── Lifecycle capability ──────────────────────────────────────────
//
// Eighth capability layer: per-domain rollout strategy. Where
// Layer 4 (DependentDomain) declares **apply X before Y**, Layer
// 8 declares **when X changes, here's how to swap it**.
//
// Different shapes need different protocols:
//   - service-shaped CRs (Gateway, Service): RollingUpdate
//   - stateful resources (ConfigMaps owned by stateful sets):
//     Recreate
//   - kernel-attached programs (eBPF): BlueGreen — load new
//     before unloading old, atomic-swap (the verifier rejects
//     half-loaded state, so blue/green is the only safe shape)
//   - config CRs (most CRD-shaped resources): Immediate
//
// `tatara-rollout` (and future `tatara-deploy`) consult this
// per Change to pick the right swap protocol for each resource.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RolloutStrategy {
    /// Apply once, no transition. Most config-shaped CRDs.
    Immediate,
    /// Tear down, then create. Stateful resources where in-place
    /// updates aren't safe.
    Recreate,
    /// Standard rolling update — replace pod-by-pod with health
    /// probes between. Service-shaped CRs.
    RollingUpdate,
    /// Install new alongside old, switch traffic, drain old.
    /// Kernel-attached programs (eBPF) — the verifier won't
    /// accept half-loaded state, so blue/green is the only
    /// safe shape.
    BlueGreen,
    /// Percentage traffic shift over time. Service mesh primary
    /// pattern.
    Canary,
}

pub trait LifecycleProtocol {
    /// How changes to this domain's resources roll out.
    const STRATEGY: RolloutStrategy;
    /// Seconds to wait for graceful termination before force-kill.
    /// 30s default matches K8s pod terminationGracePeriodSeconds.
    const DRAIN_SECONDS: u32 = 30;
}

#[derive(Clone, Copy, Debug)]
pub struct LifecycleHandler {
    pub keyword: &'static str,
    pub strategy: RolloutStrategy,
    pub drain_seconds: u32,
}

static LIFECYCLE_REGISTRY: OnceLock<Mutex<HashMap<&'static str, LifecycleHandler>>> =
    OnceLock::new();

fn lifecycle_registry() -> &'static Mutex<HashMap<&'static str, LifecycleHandler>> {
    LIFECYCLE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_lifecycle<T>()
where
    T: TataraDomain + LifecycleProtocol,
{
    let handler = LifecycleHandler {
        keyword: T::KEYWORD,
        strategy: T::STRATEGY,
        drain_seconds: T::DRAIN_SECONDS,
    };
    lifecycle_registry()
        .lock()
        .unwrap()
        .insert(T::KEYWORD, handler);
}

#[must_use]
pub fn lookup_lifecycle(keyword: &str) -> Option<LifecycleHandler> {
    lifecycle_registry().lock().unwrap().get(keyword).copied()
}

#[must_use]
pub fn registered_lifecycle_keywords() -> Vec<&'static str> {
    lifecycle_registry()
        .lock()
        .unwrap()
        .keys()
        .copied()
        .collect()
}

// ── Meta-compounder: capability_layer! macro ──────────────────────
//
// Layers 1–8 above each take ~50 lines of boilerplate (trait +
// handler struct + registry + 3 fns). The macro below collapses
// every static-data capability layer to ~10 lines of declaration.
// First-class compounding the compounding: each new layer is now
// shorter to author than its predecessors.
//
// Use the macro for layers whose trait holds only `const` items
// (and whose handler is a flat struct of those values). Layers
// with executable behavior (Validated, layer 7) keep the
// hand-written form because the trait carries a method, not
// constants — `fn validate_value(&Value) -> Result<…>` doesn't
// fit a `const` slot.
//
// Shape:
//
//   capability_layer! {
//       trait $Trait,                     // pub trait + name
//       handler $Handler,                 // erased Handler struct
//       static $REGISTRY,                 // backing OnceLock
//       registry_fn $internal_fn,         // private accessor
//       register $register_fn,            // pub register::<T>()
//       lookup $lookup_fn,                // pub lookup(kw) -> Option<Handler>
//       list $list_fn,                    // pub list registered keywords
//       consts {
//           const NAME: ty => field name,  // trait const → handler field
//           ...
//       }
//   }

#[macro_export]
macro_rules! capability_layer {
    (
        trait $Trait:ident,
        handler $Handler:ident,
        static $REGISTRY:ident,
        registry_fn $registry_fn:ident,
        register $register:ident,
        lookup $lookup:ident,
        list $list:ident,
        consts {
            $(const $CONST:ident: $ty:ty => field $field:ident),* $(,)?
        } $(,)?
    ) => {
        pub trait $Trait {
            $(const $CONST: $ty;)*
        }

        #[derive(Clone, Copy, Debug)]
        pub struct $Handler {
            pub keyword: &'static str,
            $(pub $field: $ty,)*
        }

        static $REGISTRY: ::std::sync::OnceLock<
            ::std::sync::Mutex<::std::collections::HashMap<&'static str, $Handler>>
        > = ::std::sync::OnceLock::new();

        fn $registry_fn() -> &'static ::std::sync::Mutex<
            ::std::collections::HashMap<&'static str, $Handler>
        > {
            $REGISTRY.get_or_init(|| {
                ::std::sync::Mutex::new(::std::collections::HashMap::new())
            })
        }

        pub fn $register<T>()
        where
            T: $crate::domain::TataraDomain + $Trait,
        {
            let handler = $Handler {
                keyword: T::KEYWORD,
                $($field: T::$CONST,)*
            };
            $registry_fn().lock().unwrap().insert(T::KEYWORD, handler);
        }

        #[must_use]
        pub fn $lookup(keyword: &str) -> Option<$Handler> {
            $registry_fn().lock().unwrap().get(keyword).copied()
        }

        #[must_use]
        pub fn $list() -> Vec<&'static str> {
            $registry_fn().lock().unwrap().keys().copied().collect()
        }
    };
}

// ── Layer 9: Compliant capability (via the macro) ─────────────────
//
// First layer authored with the meta-compounder. Compounding the
// compounding made operational. Per-domain compliance posture —
// which baselines the resource satisfies (NIST 800-53, CIS,
// FedRAMP, PCI DSS, SOC 2). Consumers: kensa (compliance engine),
// sekiban (admission webhook), tameshi (heartbeat chain).

capability_layer! {
    trait CompliantDomain,
    handler ComplianceHandler,
    static COMPLIANCE_REGISTRY,
    registry_fn compliance_registry,
    register register_compliance,
    lookup lookup_compliance,
    list registered_compliance_keywords,
    consts {
        const FRAMEWORKS: &'static [&'static str] => field frameworks,
        const CONTROLS: &'static [&'static str] => field controls,
    }
}

// ── Layer 10: Observable capability (via the macro) ───────────────
//
// Per-domain Prometheus metric prefix + log label names.
// Consumers: arch-synthesizer (auto-generates ServiceMonitor +
// PodMonitor specs that scrape the right prefixes) and the
// Loki query layer (knows which labels each domain emits).

capability_layer! {
    trait ObservableDomain,
    handler ObservabilityHandler,
    static OBSERVABILITY_REGISTRY,
    registry_fn observability_registry,
    register register_observability,
    lookup lookup_observability,
    list registered_observability_keywords,
    consts {
        const METRIC_PREFIX: &'static str => field metric_prefix,
        const LOG_LABELS: &'static [&'static str] => field log_labels,
    }
}

// ── Layer 11: Authoring help capability (via the macro) ───────────
//
// Per-domain authoring examples + a one-liner mnemonic for the
// catalog browser. Consumers: tatara-doc (renders examples in
// the catalog), IDE hover-help, the future `tatara init` CLI
// that scaffolds new programs from examples.

capability_layer! {
    trait HelpDomain,
    handler HelpHandler,
    static HELP_REGISTRY,
    registry_fn help_registry,
    register register_help,
    lookup lookup_help,
    list registered_help_keywords,
    consts {
        const MNEMONIC: &'static str => field mnemonic,
        const EXAMPLES: &'static [&'static str] => field examples,
    }
}

// ── Layer 12: Stable capability (via the macro) ───────────────────
//
// Per-domain stability signal. Consumers: caixa-lint (warns on
// unstable usages), tatara-doc (decorates the catalog), CI
// gates (blocks promotion to prod when an unstable resource
// crosses a `:tier "prod"` env boundary).

capability_layer! {
    trait StableDomain,
    handler StabilityHandler,
    static STABILITY_REGISTRY,
    registry_fn stability_registry,
    register register_stability,
    lookup lookup_stability,
    list registered_stability_keywords,
    consts {
        const STABILITY: &'static str => field stability,
        const SINCE_VERSION: &'static str => field since_version,
    }
}

// ── Meta-meta-compounder: impl_default_capabilities! ──────────────
//
// Forge-generated domains plug into the platform with a single
// macro call:
//
//   impl_default_capabilities!(MyDomainSpec);
//
// Expands to default `impl` blocks for every static-data
// capability layer that *has* a meaningful default. Layers
// without a sensible default (Render, Validated — Render needs
// real api_version+kind, Validated has its trait-default
// `validate_value`) are skipped here; the forge emits those
// separately when CRD metadata is available.
//
// **Why this matters**: previously, adding a new capability
// layer required editing both `tatara-lisp::domain` (define the
// layer) AND `tatara-domain-forge::emit` (emit per-layer impl
// blocks). Now the forge's emit is a single line; new layers
// land in this macro alone. Compounding the compounding the
// compounding — three orders deep.

#[macro_export]
macro_rules! impl_default_capabilities {
    ($Spec:ty) => {
        // NOTE: Layer 3 (Documented) is intentionally NOT here.
        // Forge-generated domains emit it explicitly with real
        // docs from CRD descriptions; hand-written domains
        // override directly. The macro covering it would create
        // a double-impl conflict in both cases.
        //
        // Layer 4 — Dependent (forge default empty).
        impl $crate::domain::DependentDomain for $Spec {
            const DEPENDS_ON: &'static [&'static str] = &[];
        }
        // Layer 7 — Validated (uses the trait's default fn).
        impl $crate::domain::ValidatedDomain for $Spec {}
        // Layer 8 — Lifecycle (Immediate is the safe CRD default).
        impl $crate::domain::LifecycleProtocol for $Spec {
            const STRATEGY: $crate::domain::RolloutStrategy =
                $crate::domain::RolloutStrategy::Immediate;
        }
        // Layer 9 — Compliance (claims none by default).
        impl $crate::domain::CompliantDomain for $Spec {
            const FRAMEWORKS: &'static [&'static str] = &[];
            const CONTROLS: &'static [&'static str] = &[];
        }
        // Layer 10 — Observable (no metrics by default).
        impl $crate::domain::ObservableDomain for $Spec {
            const METRIC_PREFIX: &'static str = "";
            const LOG_LABELS: &'static [&'static str] = &[];
        }
        // Layer 11 — Authoring help.
        impl $crate::domain::HelpDomain for $Spec {
            const MNEMONIC: &'static str = "";
            const EXAMPLES: &'static [&'static str] = &[];
        }
        // Layer 12 — Stability (assume stable + 0.1.0 unless
        // overridden; loud-failure beats silent missing field).
        impl $crate::domain::StableDomain for $Spec {
            const STABILITY: &'static str = "stable";
            const SINCE_VERSION: &'static str = "0.1.0";
        }
    };
}

/// Companion to `impl_default_capabilities!` — registers every
/// layer's handler in one call. Domains that have explicit
/// Render + Schema + Attest metadata also call those register
/// fns separately (they're not part of this macro because not
/// every domain has them — hand-written ebpf doesn't have render
/// metadata). Adding a new always-present layer means updating
/// this macro and `impl_default_capabilities!` once.
///
/// Expands to an **expression** of type `Result<(), KeywordCollision>`, not a
/// statement block: the handler registry is the only one of the nine layers
/// that can refuse, and swallowing that refusal inside the macro would put the
/// silent overwrite back one level down where it is harder to see. The eight
/// capability layers still overwrite — they carry metadata about a keyword
/// whose ownership the handler registry has already adjudicated, so a second
/// writer there cannot change which struct a form compiles to.
#[macro_export]
macro_rules! register_all_capabilities {
    ($Spec:ty) => {{
        $crate::domain::register_doc::<$Spec>();
        $crate::domain::register_deps::<$Spec>();
        $crate::domain::register_validate::<$Spec>();
        $crate::domain::register_lifecycle::<$Spec>();
        $crate::domain::register_compliance::<$Spec>();
        $crate::domain::register_observability::<$Spec>();
        $crate::domain::register_help::<$Spec>();
        $crate::domain::register_stability::<$Spec>();
        $crate::domain::register::<$Spec>()
    }};
}

// ── Sexp ↔ serde_json bridge (universal type support) ──────────────
//
// Lets the derive macro fall through to `serde_json::from_value` for any
// field type implementing `Deserialize`. Handles enums (via symbol→string),
// nested structs (via kwargs→object), and `Vec<T>` of either.

use serde_json::Value as JValue;

/// Thin delegate to [`Sexp::to_json`] retained for callers that want
/// the free-function reach — the canonical site is now the inherent
/// method on the [`Sexp`] algebra (sibling-lift posture to
/// [`super::domain::sexp_shape`] → [`Sexp::shape`] (commit 121bb60)
/// and [`super::domain::sexp_witness`] → [`Sexp::witness`] (commit
/// a427e3b)). Rules + round-trip semantics live at
/// [`Sexp::to_json`]'s docstring.
///
/// Composition law: `sexp_to_json(s) == s.to_json()` for every `s:
/// &Sexp`. Pre-lift the dispatcher lived here as the canonical site;
/// post-lift the inherent method [`Sexp::to_json`] is the canonical
/// site and this free function delegates so existing callers
/// continue to compile.
pub fn sexp_to_json(s: &Sexp) -> Result<JValue> {
    s.to_json()
}

/// Thin delegate to [`Sexp::from_json`] retained for callers that want
/// the free-function reach — the canonical site is now the inherent
/// associated function on the [`Sexp`] algebra (sibling-lift posture to
/// [`super::domain::sexp_to_json`] → [`Sexp::to_json`] (commit 875ee3b),
/// [`super::domain::sexp_witness`] → [`Sexp::witness`] (commit a427e3b),
/// and [`super::domain::sexp_shape`] → [`Sexp::shape`] (commit
/// 121bb60)). Rules + round-trip semantics live at
/// [`Sexp::from_json`]'s docstring.
///
/// Composition law: `json_to_sexp(v) == Sexp::from_json(v)` for every
/// `v: &JValue`. Pre-lift the dispatcher lived here as the canonical
/// site; post-lift the inherent associated function
/// [`Sexp::from_json`] is the canonical site and this free function
/// delegates so existing callers continue to compile. With this lift the
/// substrate's `Sexp` ↔ `serde_json::Value` round-trip closure
/// ([`Sexp::to_json`] + [`Sexp::from_json`]) lives entirely on the
/// [`Sexp`] algebra; the four free functions that pre-dated the lift
/// chain (`sexp_to_json`, `json_to_sexp`, `sexp_shape`, `sexp_witness`)
/// are all delegates now — the canonical-form / structural-projection
/// surface is structurally on the algebra.
pub fn json_to_sexp(v: &JValue) -> Sexp {
    Sexp::from_json(v)
}

/// `must-reach` → `mustReach`, `point-type` → `pointType`.
pub(crate) fn kebab_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = false;
    for c in s.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `mustReach` → `must-reach` (inverse of `kebab_to_camel`).
pub(crate) fn camel_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// ── TypedRewriter — the self-optimization primitive ────────────────
//
// Takes a typed value, converts to Sexp, applies a Lisp rewrite, then
// re-enters the typed boundary via `compile_from_args`. Any rewrite that
// passes the typed re-validation is safe by construction — the Rust type
// system is the floor.

/// Rewrite a typed `T` through Lisp form and re-validate on the way back.
///
/// The rewriter receives the value's kwargs representation (a `Sexp::List`
/// of alternating keywords + values) and returns a modified kwargs list.
/// `T::compile_from_args` validates the result — any ill-formed rewrite
/// produces a typed error; any well-formed rewrite produces a valid `T`.
pub fn rewrite_typed<T, F>(input: T, rewrite: F) -> Result<T>
where
    T: TataraDomain + serde::Serialize,
    F: FnOnce(Sexp) -> Result<Sexp>,
{
    let json = serde_json::to_value(&input).map_err(|e| LispError::Compile {
        form: T::KEYWORD.to_string(),
        message: format!("serialize {}: {e}", T::KEYWORD),
    })?;
    let sexp = json_to_sexp(&json);
    let rewritten = rewrite(sexp)?;
    let args = match rewritten {
        Sexp::List(items) => items,
        other => {
            return Err(LispError::Compile {
                form: T::KEYWORD.to_string(),
                message: format!("rewriter must return a list; got {other}"),
            })
        }
    };
    T::compile_from_args(&args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::read;
    use serde::Serialize;
    use tatara_lisp_derive::TataraDomain as DeriveTataraDomain;

    /// Example domain authorable as Lisp — proves derive macro, trait, and
    /// registry all agree end-to-end.
    #[derive(DeriveTataraDomain, Serialize, Debug, PartialEq)]
    #[tatara(keyword = "defmonitor")]
    struct MonitorSpec {
        name: String,
        query: String,
        threshold: f64,
        window_seconds: Option<i64>,
        tags: Vec<String>,
        enabled: Option<bool>,
    }

    #[test]
    fn derive_emits_correct_keyword() {
        assert_eq!(MonitorSpec::KEYWORD, "defmonitor");
    }

    // ── The two kwarg gates, each proved to REJECT ──────────────────
    //
    // Both of these forms used to compile successfully, which is the
    // whole reason the helper layer moved: a `HashMap::insert` whose
    // return value was discarded made a repeated `:key` a silent
    // last-one-wins, and the total absence of an allowed-set check made
    // a typo'd `:key` a silent fall-through to the field's default.
    // Both are now typed rejections at the parse boundary, and both
    // tests fail (compile succeeds, `unwrap_err` panics) if either gate
    // is ever removed.

    /// G1 — a repeated `:key` is REJECTED, and the diagnostic names it.
    #[test]
    fn duplicate_kwarg_is_rejected_not_silently_last_wins() {
        let forms = read(
            r#"(defmonitor
                 :name "first"
                 :query "up"
                 :threshold 0.5
                 :name "second")"#,
        )
        .expect("reads");
        let err = MonitorSpec::compile_from_sexp(&forms[0])
            .expect_err("a repeated :name must not parse — pre-fix it silently took \"second\"");
        assert!(
            matches!(&err, LispError::DuplicateKwarg { key } if key == "name"),
            "expected DuplicateKwarg {{ key: \"name\" }}, got {err:?}"
        );
    }

    /// G1, the other half — the FIRST binding is not quietly kept either.
    /// Rejection, not a silent choice of winner, is the contract.
    #[test]
    fn duplicate_kwarg_rejects_rather_than_picking_a_winner() {
        let forms =
            read(r#"(defmonitor :name "a" :name "a" :query "q" :threshold 1.0)"#).expect("reads");
        assert!(MonitorSpec::compile_from_sexp(&forms[0]).is_err());
    }

    /// G2 — an unknown `:key` is REJECTED rather than ignored, and a
    /// near-miss carries an edit-distance hint.
    #[test]
    fn unknown_kwarg_is_rejected_with_a_suggestion() {
        let forms = read(
            r#"(defmonitor
                 :name "prom-up"
                 :query "up"
                 :threshold 0.5
                 :thrshold 0.9)"#,
        )
        .expect("reads");
        let err = MonitorSpec::compile_from_sexp(&forms[0]).expect_err(
            "a typo'd :thrshold must not parse — pre-fix it was dropped and `threshold` \
             kept whatever the correctly-spelled slot held",
        );
        let LispError::UnknownKwarg { key, hint, .. } = &err else {
            panic!("expected UnknownKwarg, got {err:?}");
        };
        assert_eq!(key, "thrshold");
        assert_eq!(
            hint.as_deref(),
            Some("threshold"),
            "a one-character transposition is inside the suggestion bound"
        );
    }

    /// G2, the far-from-anything case — still rejected, just without a
    /// hint. A wrong hint is worse than no hint.
    #[test]
    fn unknown_kwarg_with_no_near_match_is_still_rejected() {
        let forms =
            read(r#"(defmonitor :name "n" :query "q" :threshold 1.0 :zzzzzzzz 1)"#).expect("reads");
        let err = MonitorSpec::compile_from_sexp(&forms[0]).expect_err("unknown key must reject");
        let LispError::UnknownKwarg { key, hint, .. } = &err else {
            panic!("expected UnknownKwarg, got {err:?}");
        };
        assert_eq!(key, "zzzzzzzz");
        assert_eq!(hint.as_deref(), None);
    }

    // ── G3 — the RANGE gate on the four numeric arms ────────────────
    //
    // Third gate, same posture as G1/G2 above: every form in this block
    // used to COMPILE SUCCESSFULLY and put a number in the struct that
    // the author never wrote. The derive emitted `extract_int(&kw,
    // key)? as u32`, and Rust's `as` is total by truncating — it wraps,
    // sign-flips, and saturates to `inf` without a word. So the failure
    // was not a bad diagnostic; it was silent data corruption behind a
    // green build.
    //
    // Each test below names the exact wrong value the pre-fix code
    // produced, so the test doubles as the record of what regressed if
    // the narrowing is ever backed out: restore the `as` casts and
    // every one of these fails at `expect_err`.

    /// A domain whose numeric fields are all NARROWER than the reader's
    /// wide `i64` / `f64` — the shape the `as` cast corrupted. Covers
    /// both axes and both required/optional arms, i.e. all four numeric
    /// branches of `extractor_for`.
    #[derive(DeriveTataraDomain, Serialize, Debug, PartialEq)]
    #[tatara(keyword = "defnarrow")]
    struct NarrowSpec {
        port: u32,
        offset: i32,
        scale: f32,
        retries: Option<u32>,
        ratio: Option<f32>,
    }

    fn narrow_form(body: &str) -> LispError {
        let forms = read(body).expect("reads");
        NarrowSpec::compile_from_sexp(&forms[0])
            .expect_err("an out-of-range numeric literal must not parse")
    }

    /// The in-range case must still parse — a range gate that rejects
    /// valid input is worse than the truncation it replaced. Includes a
    /// value that LOSES PRECISION at `f32` (`0.1`) to pin that lossy-
    /// but-representable is accepted: an `f32` field asked for `f32`.
    #[test]
    fn narrowing_accepts_every_in_range_value_including_lossy_f32() {
        let forms = read(
            r"(defnarrow :port 8080 :offset -42 :scale 0.1 :retries 3 :ratio 2.5)",
        )
        .expect("reads");
        let spec = NarrowSpec::compile_from_sexp(&forms[0]).expect("in-range values must parse");
        assert_eq!(
            spec,
            NarrowSpec {
                port: 8080,
                offset: -42,
                #[allow(clippy::cast_possible_truncation)]
                scale: 0.1_f64 as f32,
                retries: Some(3),
                ratio: Some(2.5),
            }
        );
    }

    /// `u32` overflow. Pre-fix: `4294967296 as u32` == `0`, and the
    /// struct came back holding a port of zero.
    #[test]
    fn int_above_the_target_width_is_rejected_not_truncated() {
        let err = narrow_form(r"(defnarrow :port 4294967296 :offset 0 :scale 1.0)");
        let LispError::KwargOutOfRange {
            form,
            target,
            value,
        } = &err
        else {
            panic!("expected KwargOutOfRange, got {err:?}");
        };
        assert_eq!(form, &KwargPath::named("port"));
        assert_eq!(*target, NumericWidth::U32);
        assert_eq!(*value, NumericLiteral::Int(4_294_967_296));
    }

    /// A NEGATIVE literal into an unsigned width. Pre-fix: `-1 as u32`
    /// == `4294967295` — not a truncation but a sign flip, and the
    /// worst of the family because the resulting number looks plausible.
    #[test]
    fn negative_int_into_an_unsigned_width_is_rejected_not_sign_flipped() {
        let err = narrow_form(r"(defnarrow :port -1 :offset 0 :scale 1.0)");
        assert!(
            matches!(
                &err,
                LispError::KwargOutOfRange { target: NumericWidth::U32, value: NumericLiteral::Int(-1), .. }
            ),
            "expected a u32 range rejection of -1, got {err:?}"
        );
    }

    /// `i32` overflow — the signed sibling. Pre-fix: `2147483648 as i32`
    /// == `-2147483648`, a positive literal landing as a negative field.
    #[test]
    fn int_above_the_signed_target_width_is_rejected_not_wrapped() {
        let err = narrow_form(r"(defnarrow :port 0 :offset 2147483648 :scale 1.0)");
        assert!(
            matches!(
                &err,
                LispError::KwargOutOfRange {
                    target: NumericWidth::I32,
                    value: NumericLiteral::Int(2_147_483_648),
                    ..
                }
            ),
            "expected an i32 range rejection, got {err:?}"
        );
    }

    /// `f32` overflow. Pre-fix: `1e300 as f32` == `inf`, so a finite
    /// input became a non-finite field and every downstream arithmetic
    /// on it produced `inf`/`NaN`.
    #[test]
    fn float_above_the_target_width_is_rejected_not_saturated_to_infinity() {
        let err = narrow_form(r"(defnarrow :port 0 :offset 0 :scale 1.0e300)");
        let LispError::KwargOutOfRange { target, value, .. } = &err else {
            panic!("expected KwargOutOfRange, got {err:?}");
        };
        assert_eq!(*target, NumericWidth::F32);
        assert!(
            matches!(value, NumericLiteral::Float(x) if (*x - 1.0e300).abs() < f64::EPSILON),
            "the diagnostic must echo the author's own literal, got {value:?}"
        );
    }

    /// The OPTIONAL arms carry the same gate. A present-but-out-of-range
    /// optional is a rejection, never a quiet `None` — dropping the
    /// value would be the same corruption in a different costume.
    #[test]
    fn optional_numeric_arms_reject_out_of_range_rather_than_yielding_none() {
        let int_err =
            narrow_form(r"(defnarrow :port 0 :offset 0 :scale 1.0 :retries 4294967296)");
        assert!(
            matches!(
                &int_err,
                LispError::KwargOutOfRange { target: NumericWidth::U32, .. }
            ),
            "expected an Option<u32> range rejection, got {int_err:?}"
        );

        let float_err = narrow_form(r"(defnarrow :port 0 :offset 0 :scale 1.0 :ratio 1.0e300)");
        assert!(
            matches!(
                &float_err,
                LispError::KwargOutOfRange { target: NumericWidth::F32, .. }
            ),
            "expected an Option<f32> range rejection, got {float_err:?}"
        );
    }

    /// An ABSENT optional is still `None` — the gate fires on presence,
    /// not on the field's existence.
    #[test]
    fn absent_optional_numeric_kwargs_stay_none_under_the_range_gate() {
        let forms = read(r"(defnarrow :port 1 :offset 1 :scale 1.0)").expect("reads");
        let spec = NarrowSpec::compile_from_sexp(&forms[0]).expect("absent optionals are legal");
        assert_eq!(spec.retries, None);
        assert_eq!(spec.ratio, None);
    }

    /// The rendered diagnostic names the kwarg, the value, and the
    /// width — the three facts an author needs to fix the source. Pinned
    /// because this is the operator-facing surface, not just the variant.
    #[test]
    fn the_range_diagnostic_names_kwarg_value_and_target_width() {
        let err = narrow_form(r"(defnarrow :port 4294967296 :offset 0 :scale 1.0)");
        assert_eq!(
            err.to_string(),
            "compile error in :port: 4294967296 is out of range for u32"
        );
    }

    /// The IDENTITY widths must not have become rejections: `i64` and
    /// `f64` fields route through the same `NarrowNumeric` projection
    /// with a total impl, so `i64::MIN` still parses. `MonitorSpec`'s
    /// `threshold: f64` / `window_seconds: Option<i64>` are the fields
    /// under test — the same two the pre-fix code cast with a no-op
    /// `as i64` / `as f64`.
    #[test]
    fn identity_widths_stay_total_under_the_range_gate() {
        let forms = read(
            r#"(defmonitor :name "n" :query "q" :threshold 1.0 :window-seconds -9223372036854775808)"#,
        )
        .expect("reads");
        let spec = MonitorSpec::compile_from_sexp(&forms[0])
            .expect("i64::MIN is representable at the identity width");
        assert_eq!(spec.window_seconds, Some(i64::MIN));
    }

    /// The typed width identity is sourced from the TYPE, not from a
    /// literal the derive interpolated — `NarrowNumeric::WIDTH` is the
    /// single producer, so a mislabeled diagnostic is unconstructible
    /// rather than merely untested.
    #[test]
    fn every_supported_width_reports_its_own_typed_identity() {
        assert_eq!(<i32 as NarrowNumeric<i64>>::WIDTH, NumericWidth::I32);
        assert_eq!(<i64 as NarrowNumeric<i64>>::WIDTH, NumericWidth::I64);
        assert_eq!(<u32 as NarrowNumeric<i64>>::WIDTH, NumericWidth::U32);
        assert_eq!(<u64 as NarrowNumeric<i64>>::WIDTH, NumericWidth::U64);
        assert_eq!(<usize as NarrowNumeric<i64>>::WIDTH, NumericWidth::Usize);
        assert_eq!(<f32 as NarrowNumeric<f64>>::WIDTH, NumericWidth::F32);
        assert_eq!(<f64 as NarrowNumeric<f64>>::WIDTH, NumericWidth::F64);
    }

    /// `f32`'s narrowing rejects exactly one thing — a FINITE input that
    /// overflows — and passes an already-non-finite input through. A
    /// naive `is_finite()` guard would have rejected an authored `inf`
    /// that `as` had preserved correctly, which would be a regression
    /// dressed as a fix.
    #[test]
    fn f32_narrowing_rejects_only_finite_overflow() {
        assert_eq!(<f32 as NarrowNumeric<f64>>::narrow(1.0), Some(1.0_f32));
        assert_eq!(<f32 as NarrowNumeric<f64>>::narrow(1.0e300), None);
        assert_eq!(<f32 as NarrowNumeric<f64>>::narrow(-1.0e300), None);
        assert_eq!(
            <f32 as NarrowNumeric<f64>>::narrow(f64::INFINITY),
            Some(f32::INFINITY)
        );
        assert!(<f32 as NarrowNumeric<f64>>::narrow(f64::NAN)
            .expect("NaN passes through")
            .is_nan());
    }

    /// The gate must not over-reject: every declared field's kebab key —
    /// including the ones reached through the four extractor branches —
    /// stays accepted. This is what would fail if the allowed-set were
    /// collected inside a branch that `continue`s.
    #[test]
    fn every_declared_field_key_stays_accepted() {
        let forms = read(
            r#"(defmonitor
                 :name "n"
                 :query "q"
                 :threshold 1.0
                 :window-seconds 30
                 :tags ("a")
                 :enabled #f)"#,
        )
        .expect("reads");
        MonitorSpec::compile_from_sexp(&forms[0]).expect("all six declared keys must remain valid");
    }

    #[test]
    fn derive_compiles_full_form() {
        let forms = read(
            r#"(defmonitor
                 :name "prom-up"
                 :query "up{job='prometheus'}"
                 :threshold 0.99
                 :window-seconds 300
                 :tags ("prod" "observability")
                 :enabled #t)"#,
        )
        .unwrap();
        let spec = MonitorSpec::compile_from_sexp(&forms[0]).unwrap();
        assert_eq!(
            spec,
            MonitorSpec {
                name: "prom-up".into(),
                query: "up{job='prometheus'}".into(),
                threshold: 0.99,
                window_seconds: Some(300),
                tags: vec!["prod".into(), "observability".into()],
                enabled: Some(true),
            }
        );
    }

    #[test]
    fn derive_accepts_missing_optionals() {
        let forms = read(r#"(defmonitor :name "x" :query "q" :threshold 0.5)"#).unwrap();
        let spec = MonitorSpec::compile_from_sexp(&forms[0]).unwrap();
        assert_eq!(spec.name, "x");
        assert!(spec.window_seconds.is_none());
        assert!(spec.enabled.is_none());
        assert!(spec.tags.is_empty());
    }

    #[test]
    fn derive_errors_on_missing_required() {
        let forms = read(r#"(defmonitor :name "x" :query "q")"#).unwrap();
        assert!(MonitorSpec::compile_from_sexp(&forms[0]).is_err());
    }

    #[test]
    fn derive_errors_on_wrong_head() {
        let forms = read(r#"(not-a-monitor :name "x")"#).unwrap();
        let err = MonitorSpec::compile_from_sexp(&forms[0]).unwrap_err();
        assert!(format!("{err}").contains("expected (defmonitor"));
    }

    #[test]
    fn registry_dispatches_by_keyword() {
        register::<MonitorSpec>().expect("keyword namespace must be free in this test binary");
        assert!(registered_keywords().contains(&"defmonitor"));
        let handler = lookup("defmonitor").expect("registered");
        assert_eq!(handler.keyword, "defmonitor");
        let forms = read(r#"(ignored :name "prom" :query "q" :threshold 0.5)"#).unwrap();
        let args = forms[0].as_list().unwrap();
        let json = (handler.compile)(&args[1..]).unwrap();
        assert_eq!(json["name"], "prom");
        assert_eq!(json["query"], "q");
        assert_eq!(json["threshold"], 0.5);
    }

    // ── assert_tatara_domain_well_formed — the substrate-wide testkit ──

    #[test]
    fn assert_tatara_domain_well_formed_passes_on_the_derive_reference_impl() {
        // The reference implementor `MonitorSpec` inherits the trait
        // default `compile_from_sexp` through the derive; every one of
        // the four rejection gates (bare atom, empty list, non-symbol
        // head, wrong-head symbol) MUST fire with the substrate-wide
        // structural `LispError` variant, AND its KEYWORD `"defmonitor"`
        // MUST pass the three grammar invariants (non-empty; classifies
        // as `Atom::Symbol` via `Atom::from_lexeme`; contains no
        // `Sexp::is_bare_atom_boundary` char) AND the round-trip
        // theorem (`read("defmonitor")` projects to
        // `Some("defmonitor")`). The single line below pins all EIGHT
        // at once — every future `#[derive(TataraDomain)]` implementor
        // reduces to the same one-line check in its test module,
        // mirroring the `assert_closed_set_well_formed` deployment
        // across 44+ closed-set implementor test sites.
        assert_tatara_domain_well_formed::<MonitorSpec>();
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_empty_keyword() {
        // Negative arm on invariant (1) — a hand-written impl whose
        // KEYWORD is the empty string tries to be a keyword-less
        // dispatch target. The trait can't discriminate `(some-form
        // …)` from `(other-form …)` without a lexeme, so the testkit
        // MUST fire on this degenerate shape. Uses `catch_unwind` to
        // observe the panic without terminating the test process —
        // same posture the closed-set testkit's negative-arm tests
        // take (see `closed_set.rs`).
        struct EmptyKeyword;
        impl TataraDomain for EmptyKeyword {
            const KEYWORD: &'static str = "";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (1) trips first")
            }
        }
        let result = std::panic::catch_unwind(|| {
            assert_tatara_domain_well_formed::<EmptyKeyword>();
        });
        let payload = result.expect_err("expected empty-KEYWORD invariant to panic");
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&'static str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains("KEYWORD is empty"),
            "expected empty-KEYWORD panic message to name the invariant, got {msg:?}",
        );
    }

    /// Extract the panic message of the closure so the test module's
    /// negative-arm sweep binds to ONE substrate-owned decode instead of
    /// re-inlining the `catch_unwind` + `downcast_ref::<String>` + fallback
    /// to `&'static str` cascade at every arm.
    fn assert_panic_msg_contains(needle: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
        let result = std::panic::catch_unwind(f);
        let payload = result.expect_err("expected invariant to panic");
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&'static str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains(needle),
            "expected panic message to contain {needle:?}, got {msg:?}",
        );
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_ascii_whitespace_keyword() {
        // Negative arm on invariant (7) — a KEYWORD like `"def foo"`
        // would arrive at the trait's head-match as two tokens because
        // `Sexp::is_bare_atom_boundary(' ') == true` via
        // `char::is_whitespace`. The testkit MUST catch this before an
        // integration surface silently drops the trailing word. The
        // pre-lift ASCII-only heuristic caught the same case; the
        // sharpened invariant catches it via the substrate's typed
        // reader-boundary projection.
        struct WhitespaceKeyword;
        impl TataraDomain for WhitespaceKeyword {
            const KEYWORD: &'static str = "def foo";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (7) trips first")
            }
        }
        assert_panic_msg_contains("reader-boundary char", || {
            assert_tatara_domain_well_formed::<WhitespaceKeyword>();
        });
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_unicode_whitespace_keyword() {
        // Negative arm on invariant (7) — a KEYWORD carrying the
        // no-break-space codepoint `\u{00A0}` (a Unicode-whitespace
        // char the pre-lift `is_ascii_whitespace()` check silently
        // accepted). The reader's outer-dispatch calls
        // `char::is_whitespace()` (Unicode-aware) via
        // `Sexp::is_bare_atom_boundary`, so a KEYWORD `"def\u{00A0}foo"`
        // would split into two tokens. Binding the invariant to the
        // substrate's typed reader-boundary projection closes this hole
        // that the pre-lift ASCII-only heuristic left open.
        struct NbspKeyword;
        impl TataraDomain for NbspKeyword {
            const KEYWORD: &'static str = "def\u{00A0}foo";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (7) trips first")
            }
        }
        assert_panic_msg_contains("reader-boundary char", || {
            assert_tatara_domain_well_formed::<NbspKeyword>();
        });
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_list_open_char_keyword() {
        // Negative arm on invariant (7) — a KEYWORD like `"def(x"`
        // embeds `Sexp::LIST_OPEN` mid-lexeme; the reader's bare-atom
        // terminator disjunct fires on `(`, splitting the token so the
        // trait's head-match would see `"def"` followed by an opening
        // paren — the head-match would fire on `"def"`, silently
        // matching a DIFFERENT keyword. This is the reader-boundary
        // hole the pre-lift ASCII-whitespace heuristic silently
        // accepted; binding to `Sexp::is_bare_atom_boundary` catches
        // it structurally.
        struct ListOpenKeyword;
        impl TataraDomain for ListOpenKeyword {
            const KEYWORD: &'static str = "def(x";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (7) trips first")
            }
        }
        assert_panic_msg_contains("reader-boundary char", || {
            assert_tatara_domain_well_formed::<ListOpenKeyword>();
        });
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_comment_lead_char_keyword() {
        // Negative arm on invariant (7) — a KEYWORD like `"def;bad"`
        // embeds `Sexp::COMMENT_LEAD` mid-lexeme; the reader's outer
        // dispatch would treat `;` as the start of a line comment,
        // discarding everything after it up to newline. The trait's
        // head-match would fire on `"def"` (the token before `;`),
        // silently matching a DIFFERENT keyword. Sibling coverage to
        // the list-open arm above on the seven-terminator disjunction
        // `Sexp::NON_WHITESPACE_BARE_ATOM_TERMINATORS`.
        struct CommentLeadKeyword;
        impl TataraDomain for CommentLeadKeyword {
            const KEYWORD: &'static str = "def;bad";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (7) trips first")
            }
        }
        assert_panic_msg_contains("reader-boundary char", || {
            assert_tatara_domain_well_formed::<CommentLeadKeyword>();
        });
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_keyword_marker_prefix() {
        // Negative arm on invariant (6) — a KEYWORD `":foo"` classifies
        // as `Atom::Keyword` via the reader's `Atom::from_lexeme`
        // classifier (the `:` prefix is stripped and the remainder
        // becomes the keyword payload). The pre-lift "no leading ASCII
        // digit" heuristic silently accepted this shape; the sharpened
        // invariant binds to the substrate's typed classifier so the
        // shape rejects structurally.
        struct KeywordMarkerKeyword;
        impl TataraDomain for KeywordMarkerKeyword {
            const KEYWORD: &'static str = ":foo";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (6) trips first")
            }
        }
        assert_panic_msg_contains("Atom::from_lexeme", || {
            assert_tatara_domain_well_formed::<KeywordMarkerKeyword>();
        });
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_bool_literal_keyword() {
        // Negative arm on invariant (6) — a KEYWORD `"#t"` classifies
        // as `Atom::Bool(true)` via `Atom::from_lexeme`'s bool-literal
        // arm. The pre-lift heuristic silently accepted this shape
        // (starts with `#`, not a digit); the sharpened invariant binds
        // to the substrate's typed classifier so the shape rejects
        // structurally. Peer coverage to the `:foo` arm above on the
        // classifier's non-`Symbol` decode paths.
        struct BoolLiteralKeyword;
        impl TataraDomain for BoolLiteralKeyword {
            const KEYWORD: &'static str = "#t";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (6) trips first")
            }
        }
        assert_panic_msg_contains("Atom::from_lexeme", || {
            assert_tatara_domain_well_formed::<BoolLiteralKeyword>();
        });
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_numeric_keyword() {
        // Negative arm on invariant (6) — a KEYWORD `"42"` classifies
        // as `Atom::Int(42)` via `Atom::from_lexeme`'s `parse::<i64>`
        // arm. The pre-lift heuristic caught this via the leading-
        // digit check; the sharpened invariant catches it via the
        // classifier's typed decode — a stricter check with a
        // structurally-named diagnostic.
        struct NumericKeyword;
        impl TataraDomain for NumericKeyword {
            const KEYWORD: &'static str = "42";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                unreachable!("compile_from_args unreachable — invariant (6) trips first")
            }
        }
        assert_panic_msg_contains("Atom::from_lexeme", || {
            assert_tatara_domain_well_formed::<NumericKeyword>();
        });
    }

    #[test]
    fn assert_tatara_domain_well_formed_panics_on_drifted_compile_from_sexp() {
        // Negative arm on invariant (4) — an override that swallows
        // the bare-atom form and returns `Ok(_)` drifts the trait's
        // typed-entry gate; the testkit MUST fire on this drift so
        // the substrate-wide `NotAListForm` contract stays enforced
        // across every implementor rather than only across those that
        // keep the trait default.
        #[derive(Debug, PartialEq)]
        struct SwallowsBareAtom;
        impl TataraDomain for SwallowsBareAtom {
            const KEYWORD: &'static str = "defbogus";
            fn compile_from_args(_: &[Sexp]) -> Result<Self> {
                Ok(SwallowsBareAtom)
            }
            fn compile_from_sexp(_form: &Sexp) -> Result<Self> {
                // Intentionally-broken: this override accepts EVERY
                // form, including a bare atom — the drift the testkit
                // catches.
                Ok(SwallowsBareAtom)
            }
        }
        let result = std::panic::catch_unwind(|| {
            assert_tatara_domain_well_formed::<SwallowsBareAtom>();
        });
        let payload = result.expect_err("expected drifted-override invariant to panic");
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&'static str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains("accepted a bare-atom form"),
            "expected drifted-override panic message to name the invariant, got {msg:?}",
        );
    }
}
