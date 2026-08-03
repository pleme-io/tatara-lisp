//! `unbound-symbol` matrix.
//!
//! The rule's whole risk is FALSE POSITIVES — a lint that cries wolf gets
//! ignored, and an ignored lint costs a run while protecting nothing. So this
//! matrix is weighted toward the clean cases (every binding form, quoting,
//! ordering) and carries both polarities, enforced by
//! `matrix_has_both_polarities` below: a table that drifted to all-clean would
//! silently stop proving the rule detects anything.

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
