//! Parity harness — runs identical tatara-lisp programs through
//! both the tree-walker (`eval_program`) and the bytecode VM
//! (`eval_program_vm`) and asserts they produce the same `Value`.
//!
//! Catches VM bugs that the dedicated VM tests miss + documents the
//! VM's compatibility surface explicitly. Every entry in the
//! `PARITY_CASES` table is a snippet known to work through both
//! paths today; new VM features get a parity case as part of their
//! landing PR.
//!
//! Why a curated table instead of running every existing test?
//! Many of the eval crate's tests construct an Interpreter, install
//! the stdlib, and assert against a specific Value shape. Reusing
//! them would require parameterizing every test fn over the
//! evaluation strategy. A dedicated parity table is a smaller,
//! more readable surface and the test bodies share a single
//! comparison harness.
//!
//! ## VM-closures-into-native-HoFs (Phase 6 — resolved)
//!
//! VM-compiled closures (`Value::Foreign(CompiledClosure)`) flow into
//! native higher-order primitives (`map`, `filter`, `foldl`, …)
//! through `Caller::apply_value`, which routes through the
//! tree-walker's `apply()`. `apply()` recognizes the Foreign-tagged
//! variant and lifts it back to a tree-walker `Closure` for
//! dispatch — see `CompiledClosure::lift_to_closure` for details.
//!
//! Trade-off: the lifted invocation runs through the tree-walker, not
//! the VM. Correctness is the same (the VM is parity-validated), and
//! the alternative (re-entering the VM from inside a native HoF
//! callback) would require threading mutable Interpreter state
//! through `Caller` — a deeper refactor we may revisit if profiling
//! shows the lift cost matters for embedder workloads.
//!
//! Mutation note: `set!` inside a closure invoked through the lift
//! path writes to the lifted captured env, NOT to the original VM
//! upvalue cells. Closures that need shared `set!` semantics with
//! their VM-side siblings should be invoked from VM contexts
//! directly (which is the common case — HoF callbacks rarely
//! mutate captures).

#[cfg(test)]
mod tests {
    use crate::install_full_stdlib_with;
    use crate::Interpreter;
    use crate::Value;
    use tatara_lisp::read_spanned;

    struct NoHost;

    /// One parity case: a name (for failure attribution) + a Lisp
    /// source string. Both interpreters are asked to evaluate this
    /// program; their results MUST be `Display`-equal (we compare
    /// the rendered form because `Value` doesn't implement `Eq`).
    struct ParityCase {
        name: &'static str,
        src: &'static str,
    }

    /// The canonical compatibility surface. Every snippet here is
    /// known to produce IDENTICAL results from both the tree-walker
    /// and the VM. Add a row when a VM phase lands new functionality.
    const PARITY_CASES: &[ParityCase] = &[
        // ── Atoms + arithmetic ────────────────────────────────────
        ParityCase {
            name: "literal-int",
            src: "42",
        },
        ParityCase {
            name: "literal-float",
            src: "3.14",
        },
        ParityCase {
            name: "literal-bool",
            src: "#t",
        },
        ParityCase {
            name: "literal-string",
            src: "\"hello\"",
        },
        ParityCase {
            name: "literal-keyword",
            src: ":foo",
        },
        ParityCase {
            name: "arithmetic-add",
            src: "(+ 1 2 3 4 5)",
        },
        ParityCase {
            name: "arithmetic-mixed",
            src: "(+ (* 3 4) (- 10 5))",
        },
        ParityCase {
            name: "comparison",
            src: "(< 1 2 3)",
        },
        ParityCase {
            name: "modulo",
            src: "(modulo 17 5)",
        },
        // ── Conditionals + boolean logic ──────────────────────────
        ParityCase {
            name: "if-then",
            src: "(if #t 1 2)",
        },
        ParityCase {
            name: "if-else",
            src: "(if #f 1 2)",
        },
        ParityCase {
            name: "and-truthy",
            src: "(and 1 2 3)",
        },
        ParityCase {
            name: "or-short-circuit",
            src: "(or #f #f 7)",
        },
        ParityCase {
            name: "not-true",
            src: "(not #t)",
        },
        // ── Variables + bindings ──────────────────────────────────
        ParityCase {
            name: "define-then-use",
            src: "(define x 42) x",
        },
        ParityCase {
            name: "define-then-set",
            src: "(define x 1) (set! x 99) x",
        },
        ParityCase {
            name: "let-binding",
            src: "(let ((x 10) (y 20)) (+ x y))",
        },
        ParityCase {
            name: "nested-let",
            src: "(let ((x 1)) (let ((y 2)) (+ x y)))",
        },
        // ── Lists ─────────────────────────────────────────────────
        ParityCase {
            name: "list-construct",
            src: "(list 1 2 3)",
        },
        ParityCase {
            name: "list-cons",
            src: "(cons 0 (list 1 2 3))",
        },
        ParityCase {
            name: "list-length",
            src: "(length (list 1 2 3 4 5))",
        },
        ParityCase {
            name: "list-reverse",
            src: "(reverse (list 1 2 3))",
        },
        ParityCase {
            name: "list-append",
            src: "(append (list 1 2) (list 3 4))",
        },
        // ── Hash maps ─────────────────────────────────────────────
        ParityCase {
            name: "hash-map-construct-and-get",
            src: "(hash-map-get (hash-map :a 1 :b 2) :a)",
        },
        ParityCase {
            name: "hash-map-set-returns-new",
            src: "(hash-map-count (hash-map-set (hash-map :a 1) :b 2))",
        },
        // ── Lambda + closures ─────────────────────────────────────
        ParityCase {
            name: "lambda-inline",
            src: "((lambda (x y) (+ x y)) 3 4)",
        },
        ParityCase {
            name: "closure-make-adder",
            src: "(define (make-adder n) (lambda (x) (+ x n)))
                  ((make-adder 10) 32)",
        },
        ParityCase {
            name: "closure-captures-let-local",
            src: "(let ((x 10)) ((lambda (y) (+ x y)) 5))",
        },
        ParityCase {
            name: "closure-captures-chain",
            src: "(let ((x 5))
                    (let ((f (lambda (a) (lambda (b) (+ x a b)))))
                      ((f 3) 4)))",
        },
        // ── Recursion + TCO ───────────────────────────────────────
        ParityCase {
            name: "recursion-factorial",
            src: "(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))
                  (fact 6)",
        },
        ParityCase {
            name: "tco-deep-loop",
            src: "(define (loop n) (if (= n 0) :done (loop (- n 1))))
                  (loop 10000)",
        },
        // ── Higher-order primitives ───────────────────────────────
        // VM-compiled closures (Foreign(CompiledClosure)) flow into
        // native HoFs through `apply()`'s lift-to-Closure path —
        // see `CompiledClosure::lift_to_closure`. Both lambda
        // literals and top-level user-defined fns work.
        ParityCase {
            name: "map-square-lambda",
            src: "(map (lambda (x) (* x x)) (list 1 2 3 4))",
        },
        ParityCase {
            name: "filter-evens-lambda",
            src: "(filter (lambda (x) (= 0 (modulo x 2))) (list 1 2 3 4 5))",
        },
        ParityCase {
            name: "map-with-toplevel-fn",
            src: "(define (sqr x) (* x x))
                  (map sqr (list 1 2 3 4))",
        },
        ParityCase {
            name: "filter-with-toplevel-fn",
            src: "(define (even? x) (= 0 (modulo x 2)))
                  (filter even? (list 1 2 3 4 5))",
        },
        ParityCase {
            name: "foldl-sum-native",
            src: "(foldl + 0 (list 1 2 3 4 5))",
        },
        ParityCase {
            name: "foldr-sum-native",
            src: "(foldr + 0 (list 1 2 3 4 5))",
        },
        ParityCase {
            name: "reduce-product-native",
            src: "(reduce * 1 (list 1 2 3 4 5))",
        },
        ParityCase {
            name: "foldl-with-lambda",
            src: "(foldl (lambda (acc x) (+ acc (* x x))) 0 (list 1 2 3 4))",
        },
        ParityCase {
            name: "map-then-filter-pipeline",
            src: "(filter (lambda (x) (> x 4))
                          (map (lambda (x) (* x x))
                               (list 1 2 3 4)))",
        },
        // ── try/catch ─────────────────────────────────────────────
        ParityCase {
            name: "try-no-throw",
            src: "(try (+ 1 2) (catch (e) :unreachable))",
        },
        ParityCase {
            name: "try-catches-throw",
            src: "(try
                    (throw (ex-info \"boom\" (list)))
                    (catch (e) (error-message e)))",
        },
        ParityCase {
            name: "try-catches-runtime",
            src: "(try (/ 1 0) (catch (e) (error-tag e)))",
        },
        // ── Tree-walker fallback (eval, quasi-quote, macroexpand) ──
        ParityCase {
            name: "fallback-eval-quoted",
            src: "(eval (quote (+ 1 2 3)))",
        },
        ParityCase {
            name: "fallback-quasi-quote-with-global",
            src: "(define x 99) `(a ,x c)",
        },
        // ── Channels ──────────────────────────────────────────────
        ParityCase {
            name: "channel-send-recv",
            src: "(define ch (chan))
                  (>! ch 1)
                  (>! ch 2)
                  (list (<! ch) (<! ch))",
        },
        // ── Type system ───────────────────────────────────────────
        ParityCase {
            name: "type-the-passes",
            src: "(the :int 42)",
        },
        ParityCase {
            name: "type-of-keyword",
            src: "(type-of :foo)",
        },
        ParityCase {
            name: "type-is-true-on-match",
            src: "(is? 42 :int)",
        },
    ];

    fn eval_tree(name: &str, src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost)
            .unwrap_or_else(|e| panic!("tree-walker failed on case {name}: {e:?}"))
    }

    fn eval_vm(name: &str, src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let forms = read_spanned(src).unwrap();
        i.eval_program_vm(&forms, &mut NoHost)
            .unwrap_or_else(|e| panic!("vm failed on case {name}: {e:?}"))
    }

    /// The single parity test. One assert per case; on mismatch the
    /// failure message names the case + shows both rendered values.
    /// Failing fast (per case) is intentional — fixing a parity bug
    /// usually involves reproducing that exact case.
    #[test]
    fn parity_across_paths() {
        let mut failures: Vec<(String, String, String)> = Vec::new();
        for case in PARITY_CASES {
            let tree = eval_tree(case.name, case.src);
            let vm = eval_vm(case.name, case.src);
            let tree_str = format!("{tree}");
            let vm_str = format!("{vm}");
            if tree_str != vm_str {
                failures.push((case.name.to_string(), tree_str, vm_str));
            }
        }
        if !failures.is_empty() {
            let msg = failures
                .into_iter()
                .map(|(n, t, v)| format!("  {n:30} tree={t:?} vm={v:?}"))
                .collect::<Vec<_>>()
                .join("\n");
            panic!(
                "VM parity failures in {} cases:\n{msg}",
                PARITY_CASES.len()
            );
        }
    }

    /// Sanity: the table covers a meaningful surface. If anyone
    /// shrinks it accidentally this test catches the regression.
    #[test]
    fn parity_table_has_minimum_coverage() {
        // Floor: 30+ cases covering all major feature categories.
        assert!(
            PARITY_CASES.len() >= 30,
            "parity table shrunk to {} cases — keep coverage broad",
            PARITY_CASES.len()
        );
    }

}
