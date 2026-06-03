//! Verification matrix for the lint rules. Each row is a (name, source,
//! expected-violation-count) triple; one test exercises every row and reports
//! every failing row at once. Adding a rule adds rows here.

use tatara_lisp_lint::{default_rules, lint_source};

fn count(src: &str) -> usize {
    lint_source(src, &default_rules())
        .unwrap_or_else(|e| panic!("source did not parse: {e}\n--- src ---\n{src}"))
        .len()
}

struct Row {
    name: &'static str,
    src: &'static str,
    expect: usize,
}

const MATRIX: &[Row] = &[
    // ── git-mutation-discard: should FLAG ──────────────────────────────
    Row {
        name: "discarded top-level git commit",
        src: r#"(exec-check "git" "commit" "-m" "x")"#,
        expect: 1,
    },
    Row {
        name: "discarded top-level git tag + push",
        src: "(exec-check \"git\" \"tag\" \"-a\" \"v1\" \"-m\" \"x\")\n(exec-capture \"git\" \"push\" \"origin\" \"main\")",
        expect: 2,
    },
    Row {
        name: "discarded git commit as non-last in fn body",
        src: r#"(define (f) (exec-check "git" "commit" "-m" "x") 0)"#,
        expect: 1,
    },
    Row {
        name: "discarded git push in a let body (non-tail)",
        src: r#"(let ((x 1)) (exec-capture "git" "push" "origin" "main") x)"#,
        expect: 1,
    },
    // ── git-mutation-discard: should ALLOW ─────────────────────────────
    Row {
        name: "bound result",
        src: r#"(define s (exec-check "git" "push" "origin" "main"))"#,
        expect: 0,
    },
    Row {
        name: "checked inside when/=",
        src: r#"(when (not (= 0 (exec-check "git" "push" "origin" "main"))) (exit 1))"#,
        expect: 0,
    },
    Row {
        name: "exec-or-die wrapper (no exec-check head)",
        src: r#"(exec-or-die (list "git" "commit" "-m" "x"))"#,
        expect: 0,
    },
    Row {
        name: "non-mutating git (config / status) + per-path add lambda",
        src: "(exec-check \"git\" \"config\" \"user.name\" \"bot\")\n(exec-capture \"git\" \"status\")\n(for-each (lambda (p) (exec-check \"git\" \"add\" p)) (list \"a\" \"b\"))",
        expect: 0,
    },
    Row {
        name: "suppression comment on preceding line",
        src: ";; lint:allow git-mutation-discard intentional fire-and-forget\n(exec-check \"git\" \"push\" \"origin\" \"main\")",
        expect: 0,
    },
    Row {
        name: "quoted data is never a call",
        src: r#"(define forms '(exec-check "git" "commit" "-m" "x"))"#,
        expect: 0,
    },
    Row {
        name: "non-git/gh command discarded (out of these rules' scope)",
        src: r#"(exec-check "npm" "install" "-g" "snyk")"#,
        expect: 0,
    },
    // ── gh-mutation-discard: should FLAG ───────────────────────────────
    Row {
        name: "discarded gh pr create",
        src: r#"(exec-capture "gh" "pr" "create" "--title" "x" "--repo" "o/r")"#,
        expect: 1,
    },
    Row {
        name: "discarded gh release create + secret set",
        src: "(exec-check \"gh\" \"release\" \"create\" \"v1\")\n(exec-capture \"gh\" \"secret\" \"set\" \"TOKEN\" \"--body\" \"v\")",
        expect: 2,
    },
    // ── gh-mutation-discard: should ALLOW ──────────────────────────────
    Row {
        name: "bound gh pr create result",
        src: r#"(let ((r (exec-capture "gh" "pr" "create" "--title" "x"))) (status-of r))"#,
        expect: 0,
    },
    Row {
        name: "read-only gh (pr view / issue list) discarded",
        src: "(exec-capture \"gh\" \"pr\" \"view\" \"1\")\n(exec-capture \"gh\" \"issue\" \"list\")",
        expect: 0,
    },
    Row {
        name: "secondary gh edits (pr comment/edit/close) excluded for now",
        src: "(exec-check \"gh\" \"pr\" \"comment\" \"1\")\n(exec-check \"gh\" \"pr\" \"close\" \"1\")",
        expect: 0,
    },
    Row {
        name: "gh-mutation suppression comment",
        src: ";; lint:allow gh-mutation-discard best-effort notification\n(exec-capture \"gh\" \"issue\" \"create\" \"--title\" \"x\")",
        expect: 0,
    },
];

#[test]
fn matrix() {
    let mut failures = Vec::new();
    for row in MATRIX {
        let got = count(row.src);
        if got != row.expect {
            failures.push(format!(
                "  - {}: expected {} violation(s), got {}",
                row.name, row.expect, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} matrix row(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn matrix_has_both_polarities() {
    let flags = MATRIX.iter().filter(|r| r.expect > 0).count();
    let allows = MATRIX.iter().filter(|r| r.expect == 0).count();
    assert!(flags >= 3, "need ≥3 FLAG rows, have {flags}");
    assert!(allows >= 3, "need ≥3 ALLOW rows, have {allows}");
}
