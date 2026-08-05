//! **Where is this program on tatara-lisp's gradual continuum, and what put
//! it there?** — the `monosashi` reading for `tatara-lisp`.
//!
//! [`build_check`](crate::build_check) answers "is anything wrong?". This
//! answers the question underneath it: *how much of this program is the
//! checker actually able to check, and which specific forms decide that?*
//! Gradual typing makes the second question load-bearing — a clean
//! `check_program` over a corpus with no annotations says very little, because
//! `StaticType::Any` conforms in both directions, and nothing in the output
//! distinguishes "checked and fine" from "not checked at all".
//!
//! # Additive, by construction
//!
//! Nothing here changes [`check_program`](crate::build_check::check_program) —
//! not its signature, not its return type, not its behaviour. It is *pinned
//! cross-repo*: `blue` git-deps `tatara-lisp-eval` at a rev and calls it. This
//! module is a new module with new public items that CALLS the pinned function
//! and reads its `Vec<TypeDiagnostic>` from the outside, so `blue` at its
//! current pin is unaffected by the whole file existing.
//!
//! # What is measured, exactly
//!
//! Everything below is a count over forms that were really there — no
//! heuristic over source text, no estimate.
//!
//! * **considered** — top-level-or-nested `(define …)` forms with an
//!   extractable name. `define` and not the wider `def…` family, because
//!   `define` is the only definition shape this checker types:
//!   `check_define` types it, `definition_signature` gives it an arrow, and
//!   `(declare …)` pairs with it. `check_def_family` merely recurses into a
//!   `def…` body, so counting `defmacro`/`deftest` here would put subjects in
//!   the denominator the type pass never examines.
//! * **qualified** — those whose name is stated by a `(declare name type)`
//!   anywhere in the program. `check_program`'s first pass collects
//!   definitions up front precisely so a `declare` may follow its `define`, so
//!   this census does the same rather than depending on order.
//! * **analysed** — annotation sites the gradual pass consumed: every
//!   `(the type expr)` plus every `(declare name type)`. It is `0` **exactly**
//!   at the `untyped` rung, which is the promise being reported: an
//!   unannotated program buys no type analysis. (Arity checking still runs —
//!   it is not annotation-driven, which is the entire reason it survives an
//!   unannotated corpus. It is therefore not part of this number.)
//!
//! Quoted forms are skipped, via the same
//! [`QUOTE_HEADS`](tatara_lisp::binding_shapes::QUOTE_HEADS) table
//! `check_form` reads: `'(define x 1)` is a list that happens to start with a
//! symbol, not a definition. Reading the shared table rather than spelling the
//! heads again is the point of that table existing — a second copy would be
//! free to disagree with the checker about what counts as data.
//!
//! # Stated limit: this reads RAW forms
//!
//! Like `check_program`, this census runs before macro expansion. The stdlib's
//! `defn-typed` macro states argument and return types in its signature and
//! expands to `define` + `the`, but *unexpanded* it is a `def…` head whose
//! types no pass has seen yet — so a `defn-typed` function is not in
//! `considered` and its types are not in `analysed`. That is the honest
//! reading of what the checker knows at this point, and both this limit and
//! `build_check`'s residual false-positive class dissolve at the same place:
//! running over macro-EXPANDED forms.

use monosashi::{
    Blindspot, ByteRange, Evidence, Factor, FactorKind, Ladder, Measured, Reading, Step,
};
use tatara_lisp::binding_shapes::{DEFINE_HEADS, QUOTE_HEADS};
use tatara_lisp::{Span, Spanned, SpannedForm};

use crate::build_check::{check_program, TypeDiagnostic, TypeDiagnosticKind};

/// tatara-lisp's gradual-typing continuum, loosest first.
///
/// Three rungs, because three is what this checker can actually distinguish.
/// A fourth invented to match another lisp's ladder would be a position no
/// measurement here can justify.
pub static TATARA_LADDER: Ladder = Ladder::new(
    "tatara-lisp gradual typing",
    &[
        Step::new(
            "untyped",
            "no annotation anywhere — every expression infers `:any`, which conforms both ways, so only argument COUNTS are checked",
        ),
        Step::new(
            "annotated",
            "some annotations exist; definitions without one infer `:any` and conform to everything",
        ),
        Step::new(
            "checked",
            "every definition has a declared type, so the conformance walk reaches all of them",
        ),
    ],
);

const UNTYPED: usize = 0;
const ANNOTATED: usize = 1;
const CHECKED: usize = 2;

/// What kind of form moved (or held) the reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TataraFactor {
    /// A `(declare name type)` — states a definition's type, so the
    /// conformance walk reaches it. Shifts forward.
    Declaration,
    /// A `(the type expr)` — an inline annotation on one expression. Shifts
    /// forward, but does not by itself make any *definition* checked.
    Annotation,
    /// A `(define …)` with no `(declare …)` naming it. Holds the program back,
    /// and naming it is the actionable half: "you are at `annotated` because
    /// of THIS one".
    UndeclaredDefinition,
    /// An inferred type contradicting a declared one. Holds back — the
    /// annotation is present but not yet satisfied.
    Mismatch,
    /// A malformed type spec. Holds back: the annotation was written and the
    /// checker could not read it, so it bought nothing.
    BadTypeSpec,
    /// A call passing the wrong NUMBER of arguments. Holds back. The one
    /// diagnostic that does not depend on annotations at all.
    Arity,
}

impl FactorKind for TataraFactor {
    fn label(self) -> &'static str {
        match self {
            TataraFactor::Declaration => "declaration",
            TataraFactor::Annotation => "annotation",
            TataraFactor::UndeclaredDefinition => "undeclared definition",
            TataraFactor::Mismatch => "type mismatch",
            TataraFactor::BadTypeSpec => "bad type spec",
            TataraFactor::Arity => "arity mismatch",
        }
    }

    fn shifts_forward(self) -> bool {
        matches!(self, TataraFactor::Declaration | TataraFactor::Annotation)
    }
}

/// A `tatara_lisp::Span` as `monosashi` evidence.
///
/// The synthetic sentinel (`usize::MAX..usize::MAX`, carried by every node
/// macro expansion produced) becomes a STATED blind spot rather than a byte
/// range no editor can highlight. This is the case `Evidence` exists for:
/// `Option::None` here would lose the reason and read as "no evidence needed",
/// when the truth is "this node has no source to point at".
#[must_use]
pub fn evidence_of_span(span: Span) -> Evidence {
    if span.is_synthetic() {
        Evidence::Unlocated(Blindspot::Synthetic)
    } else {
        Evidence::At(ByteRange::new(span.start, span.end))
    }
}

/// The strictness reading for a parsed program.
///
/// Calls [`check_program`](crate::build_check::check_program) unchanged and
/// reads its diagnostics from the outside; see the module docs for exactly
/// what each count in [`Measured`] means.
#[must_use]
pub fn reading_of(forms: &[Spanned]) -> Reading<TataraFactor> {
    let mut census = Census::default();
    for form in forms {
        census.walk(form);
    }

    let mut out = Reading::default();

    for (name, span) in &census.declarations {
        out.factors.push(Factor::new(
            TataraFactor::Declaration,
            name.clone(),
            evidence_of_span(*span),
            detail(name, "has a declared type, so the checker walks it"),
        ));
    }

    for (subject, span) in &census.annotations {
        out.factors.push(Factor::new(
            TataraFactor::Annotation,
            subject.clone(),
            evidence_of_span(*span),
            "an inline `(the …)` — this one expression is checked",
        ));
    }

    let mut qualified = 0usize;
    for (name, span) in &census.definitions {
        if census.declarations.iter().any(|(d, _)| d == name) {
            qualified += 1;
        } else {
            out.factors.push(Factor::new(
                TataraFactor::UndeclaredDefinition,
                name.clone(),
                evidence_of_span(*span),
                detail(name, "has no `(declare …)` — declare it to shift further"),
            ));
        }
    }

    let annotation_sites = census.annotations.len() + census.declarations.len();
    out.measured = Measured {
        analysed: annotation_sites,
        qualified,
        considered: census.definitions.len(),
    };

    // No definitions is not the bottom rung — it is no position at all.
    // "untyped" would describe a choice this author never made.
    out.rung = if census.definitions.is_empty() {
        None
    } else if annotation_sites == 0 {
        TATARA_LADDER.rung(UNTYPED)
    } else if qualified < census.definitions.len() {
        TATARA_LADDER.rung(ANNOTATED)
    } else {
        TATARA_LADDER.rung(CHECKED)
    };

    // Diagnostics are reported whether or not there is a rung: an arity error
    // in a script of bare calls is real, and dropping it because the program
    // has no position would hide it.
    for diag in check_program(forms) {
        out.factors.push(factor_of_diagnostic(&diag));
    }
    out
}

fn factor_of_diagnostic(diag: &TypeDiagnostic) -> Factor<TataraFactor> {
    let evidence = evidence_of_span(diag.span);
    match &diag.kind {
        TypeDiagnosticKind::Mismatch {
            expected,
            got,
            context,
        } => {
            let mut d = String::from("expected ");
            d.push_str(&expected.render());
            d.push_str(", got ");
            d.push_str(&got.render());
            Factor::new(TataraFactor::Mismatch, context.clone(), evidence, d)
        }
        TypeDiagnosticKind::BadTypeSpec(msg) => Factor::new(
            TataraFactor::BadTypeSpec,
            msg.clone(),
            evidence,
            "the annotation was written and the checker could not read it",
        ),
        TypeDiagnosticKind::Arity {
            expected,
            got,
            context,
        } => {
            let mut d = String::from("expected ");
            d.push_str(&expected.to_string());
            d.push_str(" argument(s), got ");
            d.push_str(&got.to_string());
            Factor::new(TataraFactor::Arity, context.clone(), evidence, d)
        }
    }
}

/// `` `name` tail `` — the one place a factor's detail line is shaped, so the
/// six kinds cannot drift into six spellings of the same sentence.
fn detail(name: &str, tail: &str) -> String {
    let mut d = String::with_capacity(name.len() + tail.len() + 3);
    d.push('`');
    d.push_str(name);
    d.push_str("` ");
    d.push_str(tail);
    d
}

/// Every `define`, `declare` and `the` the program contains, in source order.
///
/// Recursive and quote-skipping, mirroring `check_form`: that walk recurses
/// into bodies (so a nested `declare` really does bind) and returns early on
/// quoted data (so quoted forms really are data). A census with a different
/// reach would report on a program the checker did not see.
#[derive(Default)]
struct Census {
    definitions: Vec<(String, Span)>,
    declarations: Vec<(String, Span)>,
    annotations: Vec<(String, Span)>,
}

impl Census {
    fn walk(&mut self, form: &Spanned) {
        let SpannedForm::List(items) = &form.form else {
            return;
        };
        if let Some(head) = items.first().and_then(Spanned::as_symbol) {
            if QUOTE_HEADS.contains(&head) {
                return;
            }
            if head == "declare" && items.len() == 3 {
                if let Some(name) = items[1].as_symbol() {
                    self.declarations.push((name.to_string(), form.span));
                }
                return;
            }
            if head == "the" && items.len() == 3 {
                self.annotations.push((brief(&items[1]), form.span));
                self.walk(&items[2]);
                return;
            }
            if DEFINE_HEADS.contains(&head) && items.len() >= 3 {
                if let Some(name) = define_name(items) {
                    self.definitions.push((name, form.span));
                }
            }
        }
        for item in items {
            self.walk(item);
        }
    }
}

/// The name a `(define …)` introduces, in either spelling:
/// `(define (f a b) …)` and `(define f (lambda (a b) …))` — the same two
/// shapes `definition_signature` covers.
fn define_name(items: &[Spanned]) -> Option<String> {
    match &items[1].form {
        SpannedForm::List(sig) => sig.first()?.as_symbol().map(ToString::to_string),
        SpannedForm::Atom(_) => items[1].as_symbol().map(ToString::to_string),
        // `Nil` and the four quote wrappers name nothing; `(define '(x) 1)`
        // is not a definition of anything this pass can see.
        _ => None,
    }
}

/// A short label for a type form, for a factor's `subject`.
fn brief(form: &Spanned) -> String {
    form.as_symbol()
        .map_or_else(|| String::from("<type>"), ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tatara_lisp::read_spanned;

    fn reading(src: &str) -> Reading<TataraFactor> {
        let forms = read_spanned(src).expect("test source must parse");
        reading_of(&forms)
    }

    fn subjects(factors: &[&Factor<TataraFactor>]) -> Vec<String> {
        factors.iter().map(|f| f.subject.clone()).collect()
    }

    const UNANNOTATED: &str = "(define (add a b) (+ a b))";
    const DECLARED: &str = "(declare counter :int) (define counter 0)";

    /// **The default is `untyped`, and that is not a deficiency** — it is the
    /// gradual promise. The reading must show that it cost nothing.
    #[test]
    fn an_unannotated_program_is_untyped_and_buys_no_type_analysis() {
        let r = reading(UNANNOTATED);
        assert_eq!(r.rung.map(|x| x.label()), Some("untyped"));
        assert_eq!(
            r.measured.analysed, 0,
            "the promise is zero type analysis, and the reading must show it"
        );
        assert_eq!(r.measured.considered, 1);
        assert_eq!(r.measured.qualified, 0);
    }

    #[test]
    fn a_fully_declared_program_is_checked() {
        let r = reading(DECLARED);
        assert_eq!(r.rung.map(|x| x.label()), Some("checked"));
        assert!(r.measured.all_qualified());
        assert!(
            r.measured.analysed > 0,
            "declaring must buy real type analysis"
        );
    }

    /// **A mixed file is `annotated`, not `checked`.** One undeclared
    /// definition means the conformance walk genuinely did not reach it, and
    /// reporting otherwise is the reading an author would most regret
    /// trusting.
    #[test]
    fn one_undeclared_definition_holds_the_whole_program_back() {
        let r = reading("(declare counter :int) (define counter 0) (define (other x) x)");
        assert_eq!(r.rung.map(|x| x.label()), Some("annotated"), "not checked");
        assert_eq!(r.measured.qualified, 1);
        assert_eq!(r.measured.considered, 2);
    }

    /// **And it names WHICH one.** An aggregate without this is a score.
    #[test]
    fn the_reading_names_the_definition_holding_it_back() {
        let r = reading("(declare counter :int) (define counter 0) (define (other x) x)");
        let held = subjects(
            &r.holding_back()
                .into_iter()
                .filter(|f| f.kind == TataraFactor::UndeclaredDefinition)
                .collect::<Vec<_>>(),
        );
        assert_eq!(held, vec!["other"]);
    }

    /// A `declare` may FOLLOW its `define` — `check_program` collects
    /// definitions in a first pass for exactly this reason, so the census must
    /// not depend on order either.
    #[test]
    fn a_declaration_after_its_definition_still_qualifies_it() {
        let r = reading("(define counter 0) (declare counter :int)");
        assert_eq!(r.rung.map(|x| x.label()), Some("checked"));
        assert_eq!(r.measured.qualified, 1);
    }

    /// An inline `(the …)` is a real annotation — it lifts the program off
    /// `untyped` — but it does not make any DEFINITION checked, so it cannot
    /// reach `checked` on its own. The two are different axes.
    #[test]
    fn an_inline_annotation_lifts_off_untyped_but_does_not_reach_checked() {
        let r = reading("(define (add a b) (the :int (+ 1 2)))");
        assert_eq!(r.rung.map(|x| x.label()), Some("annotated"));
        assert_eq!(r.measured.qualified, 0);
        assert!(r.factors.iter().any(|f| f.kind == TataraFactor::Annotation));
    }

    /// Quoted data is data, in BOTH spellings. The `'` sugar is a
    /// `SpannedForm::Quote` wrapper the walk never descends into (exactly as
    /// `check_form` does not), and the explicit `(quote …)` list is caught by
    /// the checker's own `QUOTE_HEADS` table — reused rather than respelled,
    /// so no second list is free to disagree about what counts as data.
    #[test]
    fn a_quoted_definition_is_data_not_a_definition() {
        for src in ["(display '(define x 1))", "(display (quote (define x 1)))"] {
            let r = reading(src);
            assert_eq!(r.rung, None, "nothing was defined by {src}");
            assert_eq!(r.measured.considered, 0, "{src}");
        }
    }

    /// Nothing to measure is NOT the bottom rung.
    #[test]
    fn a_program_with_no_definitions_has_no_rung() {
        assert_eq!(reading("(+ 1 2)").rung, None);
    }

    /// …but its diagnostics are still reported. Dropping a real type error
    /// because the program has no position would hide a real defect.
    #[test]
    fn diagnostics_survive_a_program_with_no_rung() {
        let r = reading("(the :int \"oops\")");
        assert_eq!(r.rung, None, "nothing was defined");
        assert!(
            r.factors.iter().any(|f| f.kind == TataraFactor::Mismatch),
            "the mismatch must still be reported: {:?}",
            r.factors
        );
    }

    /// Every diagnostic `check_program` reports becomes a factor that HOLDS
    /// BACK — the reading and the checker cannot disagree about whether the
    /// program has a problem.
    #[test]
    fn a_declared_mismatch_is_a_factor_that_holds_back() {
        let r = reading("(declare counter :int) (define counter \"oops\")");
        let mismatches: Vec<_> = r
            .factors
            .iter()
            .filter(|f| f.kind == TataraFactor::Mismatch)
            .collect();
        assert_eq!(mismatches.len(), 1, "{:?}", r.factors);
        assert!(!mismatches[0].kind.shifts_forward());
        assert!(
            mismatches[0].detail.contains("expected"),
            "{:?}",
            mismatches[0]
        );
    }

    #[test]
    fn an_arity_error_is_a_factor_that_holds_back() {
        let r = reading("(define (add a b) (+ a b)) (add 1 2 3)");
        assert!(r.factors.iter().any(|f| f.kind == TataraFactor::Arity));
        assert!(!TataraFactor::Arity.shifts_forward());
    }

    #[test]
    fn a_malformed_type_spec_is_a_factor_that_holds_back() {
        let r = reading("(define x 1) (the (:list-of) 1)");
        assert!(
            r.factors
                .iter()
                .any(|f| f.kind == TataraFactor::BadTypeSpec),
            "{:?}",
            r.factors
        );
    }

    /// **tatara passes real evidence on day one.** This is the asymmetry that
    /// `Evidence` exists to make visible: every factor here points at real
    /// bytes, so a reading with a blind spot is a reading whose producer
    /// really has one.
    #[test]
    fn every_factor_from_real_source_carries_a_real_byte_range() {
        let src = "(declare counter :int) (define counter \"oops\") (define (other x) x)";
        let r = reading(src);
        assert!(!r.factors.is_empty());
        assert!(r.is_fully_located(), "blind spots: {:?}", r.blind_spots());
        for f in &r.factors {
            let range = f.evidence.range().expect("located");
            assert!(!range.is_empty(), "{f:?} must point at real bytes");
            assert!(
                range.end <= src.len(),
                "{f:?} must be inside the {} bytes of source",
                src.len()
            );
        }
    }

    /// A synthetic span — every node macro expansion produced carries one —
    /// becomes a STATED blind spot, not a byte range no editor can highlight
    /// and not an anonymous `None`.
    #[test]
    fn a_synthetic_span_becomes_a_stated_blind_spot() {
        assert_eq!(
            evidence_of_span(Span::synthetic()),
            Evidence::Unlocated(Blindspot::Synthetic)
        );
        assert_eq!(
            evidence_of_span(Span::new(3, 9)),
            Evidence::At(ByteRange::new(3, 9))
        );
    }

    /// Anti-vacuity: the reading must MOVE with the program. A constant would
    /// satisfy several assertions above on its own.
    #[test]
    fn the_reading_changes_as_the_program_shifts() {
        let labels: Vec<Option<&str>> = [
            "(+ 1 2)",
            UNANNOTATED,
            "(declare counter :int) (define counter 0) (define (other x) x)",
            DECLARED,
        ]
        .iter()
        .map(|s| reading(s).rung.map(|r| r.label()))
        .collect();
        assert_eq!(
            labels,
            vec![None, Some("untyped"), Some("annotated"), Some("checked")],
            "each step must move the reading"
        );
    }

    /// The ladder is three rungs and they are ordered. A reading's rung is
    /// comparable to the ladder's own positions, which is what lets a consumer
    /// ask "is this at least annotated?".
    #[test]
    fn the_ladder_is_ordered_untyped_to_checked() {
        assert_eq!(TATARA_LADDER.height(), 3);
        assert!(TATARA_LADDER.bottom() < TATARA_LADDER.top());
        assert_eq!(TATARA_LADDER.bottom().label(), "untyped");
        assert_eq!(TATARA_LADDER.top().label(), "checked");
        let r = reading(DECLARED).rung.expect("has a rung");
        assert_eq!(r, TATARA_LADDER.top());
        assert!(r >= TATARA_LADDER.rung(1).unwrap());
    }

    #[test]
    fn the_summary_is_one_line_carrying_the_ramp_and_the_denominator() {
        let line = reading(DECLARED).summary();
        assert!(line.starts_with("███"), "{line}");
        assert!(line.contains("checked"), "{line}");
        assert!(line.contains("1/1 qualified"), "{line}");
    }
}
