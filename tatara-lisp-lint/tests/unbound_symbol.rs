//! `unbound-symbol` matrix.
//!
//! The rule's whole risk is FALSE POSITIVES — a lint that cries wolf gets
//! ignored, and an ignored lint costs a run while protecting nothing. So this
//! matrix is weighted toward the clean cases (every binding form, quoting,
//! ordering) and carries both polarities, enforced by
//! `matrix_has_both_polarities` below: a table that drifted to all-clean would
//! silently stop proving the rule detects anything.

use tatara_lisp_lint::rules::{Prescription, SHAPES};
use tatara_lisp_lint::{lint_source, rules, Rule};

/// A stand-in for the interpreter-injected environment. The real caller passes
/// `reserved_head_names()` + globals; the rule cannot tell the difference, which
/// is the point of injecting it.
fn env() -> Vec<String> {
    ["display", "car", "argv", "string-split", "string-lowercase", "+", "list"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn violations(src: &str) -> Vec<String> {
    let rules: Vec<Box<dyn Rule>> = vec![Box::new(rules::unbound_symbol(env()))];
    lint_source(src, &rules)
        .expect("parses")
        .into_iter()
        .map(|v| v.message)
        .collect()
}

struct Case {
    name: &'static str,
    src: &'static str,
    /// Symbol expected to be reported, or `None` for "must be clean".
    expect: Option<&'static str>,
}

const CASES: &[Case] = &[
    // ── the bug this rule was built for ───────────────────────────────
    Case {
        name: "the measured 2026-08-02 miss: a primitive that does not exist",
        src: "(display (string-downcase \"X\"))",
        expect: Some("string-downcase"),
    },
    Case {
        name: "the same call spelled correctly",
        src: "(display (string-lowercase \"X\"))",
        expect: None,
    },
    // ── binding forms must not report ─────────────────────────────────
    Case {
        name: "define value + reference",
        src: "(define path (car (argv)))\n(display path)",
        expect: None,
    },
    Case {
        name: "define function shorthand binds name AND params",
        src: "(define (ai? line) (display line))\n(ai? \"x\")",
        expect: None,
    },
    Case {
        name: "lambda params",
        src: "(display (lambda (l) (display l)))",
        expect: None,
    },
    Case {
        name: "lambda variadic rest symbol",
        src: "(display (lambda rest (display rest)))",
        expect: None,
    },
    Case {
        name: "let binds body scope",
        src: "(let ((a 1) (b 2)) (display (+ a b)))",
        expect: None,
    },
    Case {
        name: "let* sees earlier bindings",
        src: "(let* ((a 1) (b a)) (display b))",
        expect: None,
    },
    Case {
        name: "use-before-define at top level is fine (pass 1 collects all)",
        src: "(helper)\n(define (helper) (display \"hi\"))",
        expect: None,
    },
    Case {
        name: "mutual recursion",
        src: "(define (ping) (pong))\n(define (pong) (ping))",
        expect: None,
    },
    // ── a real miss INSIDE a binding form still reports ───────────────
    Case {
        name: "unbound inside a lambda body is still caught",
        src: "(display (lambda (l) (nope l)))",
        expect: Some("nope"),
    },
    Case {
        name: "let initialiser is evaluated in the outer scope, so a self-reference is unbound",
        src: "(let ((a a)) (display a))",
        expect: Some("a"),
    },
    // ── quoting is data, never a reference ────────────────────────────
    Case {
        name: "quoted list is data",
        src: "(display '(alpha beta gamma))",
        expect: None,
    },
    Case {
        name: "quasiquote holes ARE evaluated",
        src: "(define x 1)\n(display `(a ,x))",
        expect: None,
    },
    Case {
        name: "an unbound name inside an unquote hole is caught",
        src: "(display `(a ,missing))",
        expect: Some("missing"),
    },
    // ── the shapes that caused the first fleet sweep's 422 FPs ────────
    Case {
        name: "defmacro three-part shape binds NAME and PARAMS separately",
        src: "(defmacro arrow (x step) `(,step ,x))\n(arrow 1 2)",
        expect: None,
    },
    Case {
        name: "&rest in a macro parameter list is not a reference",
        src: "(defmacro thread (x &rest steps) `(,x ,@steps))\n(thread 1)",
        expect: None,
    },
    Case {
        name: "a driver/user-macro def head is never itself reported",
        src: "(deftest \"a thing\" (display 1))",
        expect: None,
    },
    Case {
        name: "a def* form's name is bound for later call sites",
        src: "(defphase build (target) (display target))\n(build \"x\")",
        expect: None,
    },
    Case {
        name: "`fn` is a lambda alias",
        src: "(display (fn (acc t) (list acc t)))",
        expect: None,
    },
    Case {
        name: "`catch` binds its parameter list like a lambda",
        src: "(display (catch (e) (display e)))",
        expect: None,
    },
    // …and the rule must still SEE inside all of those.
    Case {
        name: "unbound inside a defmacro body still reports",
        src: "(defmacro arrow (x) (bogus-helper x))",
        expect: Some("bogus-helper"),
    },
    Case {
        name: "unbound inside an fn body still reports",
        src: "(display (fn (p) (nope p)))",
        expect: Some("nope"),
    },
    // ── conservatism ──────────────────────────────────────────────────
    Case {
        name: "keywords are not references",
        src: "(display :some-keyword)",
        expect: None,
    },
    Case {
        name: "literals are not references",
        src: "(display 1)\n(display \"s\")\n(display #t)",
        expect: None,
    },
    Case {
        name: "a program using require abstains wholesale",
        src: "(require \"m\")\n(display (whatever-m-exports))",
        expect: None,
    },
    Case {
        name: "cond/else are syntax, not references",
        src: "(cond (#t (display 1)) (else (display 2)))",
        expect: None,
    },
    // ── deeper nesting / interaction between shapes ───────────────────
    Case {
        name: "lambda nested inside a let sees both scopes",
        src: "(let ((a 1)) (display (fn (b) (list a b))))",
        expect: None,
    },
    Case {
        name: "define inside a lambda body binds locally",
        src: "(display (fn (x) (define y x)))",
        expect: None,
    },
    Case {
        name: "a let binding may shadow a primitive name",
        src: "(let ((car 1)) (display car))",
        expect: None,
    },
    Case {
        name: "nested quasiquote holes are still walked",
        src: "(define q 1)\n(display `(a `(b ,q)))",
        expect: None,
    },
    Case {
        name: "quoted data inside a quasiquote hole stays data",
        src: "(display `(a ,'(untouched-symbol)))",
        expect: None,
    },
    Case {
        name: "letrec mutual recursion is legal",
        src: "(letrec ((ping (fn () (pong))) (pong (fn () (ping)))) (ping))",
        expect: None,
    },
    Case {
        name: "letrec self-reference is legal",
        src: "(letrec ((f (fn () (f)))) (f))",
        expect: None,
    },
    Case {
        name: "symbols inside a string literal are not references",
        src: "(display \"(nope not-a-symbol)\")",
        expect: None,
    },
    Case {
        name: "empty program yields nothing",
        src: "",
        expect: None,
    },
    Case {
        name: "comment-only program yields nothing",
        src: ";; just a comment\n",
        expect: None,
    },
    Case {
        name: "deeply nested unbound is still found",
        src: "(let ((a 1)) (display (fn (b) (let ((c 2)) (deep-nope a b c)))))",
        expect: Some("deep-nope"),
    },
    Case {
        name: "suppression on the PRECEDING line works",
        src: ";; lint:allow unbound-symbol host provides it\n(host-thing)",
        expect: None,
    },
    // ── CHARACTERIZATION of the catalog's UnresolvedFalsePositive rows ────
    // These pin CURRENT WRONG behaviour on purpose. Each is a known false
    // positive with its fix recorded in the catalog; when macro expansion lands,
    // these tests FAIL, which is the signal to flip the row to Binds/Data. A
    // limitation with no test is folklore.
    Case {
        name: "RULE-ALONE macro-binding-form: dolist reports without caller expansion",
        src: "(dolist (entry (list 1)) (display entry))",
        expect: Some("entry"),
    },
    Case {
        name: "RULE-ALONE macro-dsl-data: a DSL constant reports without caller expansion",
        src: "(defreversal decouple :losses nothing)",
        expect: Some("nothing"),
    },
    Case {
        name: "file-wide suppression silences a non-self-contained file",
        src: ";;; frag.tlisp — concatenated with its other half before running.\n;; lint:allow-file unbound-symbol fragment; helpers come from the other half\n(display (helper-from-elsewhere 1))\n(display (another-one 2))",
        expect: None,
    },
    Case {
        name: "file-wide suppression must be in the HEADER, not buried mid-file",
        src: ";; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; filler\n;; lint:allow-file unbound-symbol buried, must NOT count\n(display (still-reported 1))",
        expect: Some("still-reported"),
    },
    Case {
        name: "suppression comment silences a deliberate case",
        src: ";; lint:allow unbound-symbol provided by the host at runtime\n(host-injected)",
        expect: None,
    },
];

#[test]
fn matrix() {
    for case in CASES {
        let found = violations(case.src);
        match case.expect {
            // A RULE-ALONE case characterizes what the rule reports WITHOUT the
            // caller's macro expansion. Those
            // cascade by nature — an unmodelled binding form reports its head AND
            // every use of the name it should have bound — so the assertion is
            // "at least one, naming the symbol". Pinning an exact count here
            // would make the test brittle against unrelated improvements while
            // proving nothing extra.
            Some(sym) if case.name.starts_with("RULE-ALONE") => {
                assert!(
                    found.iter().any(|m| m.contains(sym)),
                    "case `{}`: expected a violation naming `{sym}` (characterizing today's \
                     false positive), got {found:?}",
                    case.name
                );
            }
            Some(sym) => {
                assert_eq!(
                    found.len(),
                    1,
                    "case `{}`: expected exactly one violation naming `{sym}`, got {found:?}",
                    case.name
                );
                assert!(
                    found[0].contains(sym),
                    "case `{}`: violation should name `{sym}`, got {:?}",
                    case.name,
                    found[0]
                );
            }
            None => assert!(
                found.is_empty(),
                "case `{}`: expected NO violations (false positive), got {found:?}",
                case.name
            ),
        }
    }
}

/// A matrix that drifted to a single polarity would stop proving anything —
/// all-clean would pass against a rule that never fires, all-dirty against one
/// that fires on everything.
#[test]
fn matrix_has_both_polarities() {
    assert!(CASES.iter().any(|c| c.expect.is_some()), "no positive case");
    assert!(CASES.iter().any(|c| c.expect.is_none()), "no negative case");
}

// ── CATALOG COHERENCE ────────────────────────────────────────────────────────
// The shape catalog is only worth having if it cannot drift from the walker or
// from the evidence. These three tests are what make it binding rather than
// decorative — the "a variant cannot land without a row" property.

/// Every head the walker special-cases must appear in exactly one catalog row.
/// Adding a head to a match arm without cataloguing it fails here.
#[test]
fn catalog_covers_every_special_cased_head() {
    for head in rules::special_cased_heads() {
        let rows: Vec<&str> = SHAPES
            .iter()
            .filter(|s| s.heads.contains(&head))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "head `{head}` is special-cased by the walker but appears in {} catalog rows ({rows:?}); \
             every head belongs to exactly one shape",
            rows.len()
        );
    }
}

/// Every catalog row must name a matrix case that actually exists — so a shape
/// cannot be catalogued without evidence, and a renamed case cannot orphan a row.
#[test]
fn every_shape_has_a_matrix_case() {
    for shape in SHAPES {
        assert!(
            CASES.iter().any(|c| c.name == shape.covered_by),
            "catalog row `{}` cites matrix case {:?}, which does not exist",
            shape.name,
            shape.covered_by
        );
    }
}

/// A row admitting a false positive must be pinned by a POSITIVE case — the
/// wrong behaviour is characterized, so the eventual fix makes a test fail
/// loudly instead of passing silently. A limitation with no failing signal is
/// folklore.
#[test]
fn caller_dependent_and_unresolved_rows_are_characterized() {
    // A row that admits the rule alone reports — whether because a caller must
    // expand first, or because it is an outright false positive — must be pinned
    // by a POSITIVE case. Otherwise the admission is prose, and the day it stops
    // being true nothing tells us.
    let admitting: Vec<&rules::Shape> = SHAPES
        .iter()
        .filter(|s| {
            matches!(
                s.prescription,
                Prescription::ResolvedByCallerExpansion | Prescription::UnresolvedFalsePositive
            )
        })
        .collect();
    assert!(
        !admitting.is_empty(),
        "no row admits rule-alone reporting; if that became true, delete this test deliberately \
         rather than letting it pass vacuously"
    );
    for shape in admitting {
        let case = CASES
            .iter()
            .find(|c| c.name == shape.covered_by)
            .expect("covered_by exists (see every_shape_has_a_matrix_case)");
        assert!(
            case.expect.is_some(),
            "row `{}` admits the rule alone reports, so its case {:?} must assert that report",
            shape.name,
            shape.covered_by
        );
    }
}

/// The generated listing must show EVERY catalog row. A reflection surface that
/// silently omits a row is worse than none: it reads as complete coverage.
#[test]
fn catalog_listing_shows_every_row() {
    let listing = rules::CatalogListing.to_string();
    for shape in SHAPES {
        assert!(
            listing.contains(shape.name),
            "generated listing omits catalog row `{}`",
            shape.name
        );
        assert!(
            listing.contains(shape.example),
            "generated listing omits the example for `{}`",
            shape.name
        );
    }
    assert!(
        listing.contains("shapes catalogued"),
        "listing must state its own totals so a reader knows the denominator"
    );
}

/// TYPED EMISSION: no `format!()` in this crate's code. The rule's first draft
/// introduced eight, in a crate that had zero — so the ban is asserted rather
/// than trusted to review. Doc comments may still *name* it.
#[test]
fn no_format_macro_in_crate_code() {
    for entry in ["src/rules/unbound_symbol.rs", "src/rules/mutation_discard.rs", "src/lib.rs"] {
        let src = std::fs::read_to_string(entry).expect("source readable");
        for (n, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            let is_doc = trimmed.starts_with("///") || trimmed.starts_with("//!") || trimmed.starts_with("//");
            assert!(
                is_doc || !line.contains("format!"),
                "{entry}:{}: `format!()` is banned for emitted strings — build the text with \
                 String::from + push_str, or implement Display and use write!/writeln!",
                n + 1
            );
        }
    }
}

/// The rule must not depend on a hardcoded primitive table: with an EMPTY
/// environment, a known-good primitive becomes a violation. This is what pins
/// "the interpreter is the source of truth" as a behaviour rather than a comment.
#[test]
fn environment_is_injected_not_baked_in() {
    let rules: Vec<Box<dyn Rule>> = vec![Box::new(rules::unbound_symbol(Vec::<String>::new()))];
    let found = lint_source("(display 1)", &rules).expect("parses");
    assert_eq!(found.len(), 1, "an empty env must not silently pass: {found:?}");
    assert!(found[0].message.contains("display"));
}
