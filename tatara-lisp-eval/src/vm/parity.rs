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
//! ## VM-closures-into-native-HoFs
//!
//! VM-compiled closures (`Value::Foreign(CompiledClosure)`) flow into
//! native higher-order primitives (`map`, `filter`, `foldl`, …)
//! through `Caller::apply_value`, which runs them on a VM the caller
//! reuses for the whole traversal. That VM reads globals, the registry
//! and the expander through the same view the top-level VM reads the
//! interpreter through, so `set!` on a captured cell is shared and a
//! global's inline cache stays valid across calls.
//!
//! Two shapes still lift to a tree-walker `Closure`
//! (`CompiledClosure::lift_to_closure`): a body that reaches
//! `Op::EvalSexp`, whose fallback form must see the callback's locals,
//! and an interpreter with a call observer. A primitive that parks
//! inside a callback hands the park sentinel back as the call's value,
//! as the walker does: the native word's Rust frame cannot be
//! suspended, so a retry from the top would run the callback's
//! effects twice.

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
        // ── ★ THE FALLBACK-FORMS-WITH-A-LOCAL GAP ────────────────
        // Cond/When/Unless/LetStar/LetRec are VmDisposition::Fallback, so
        // the VM emits Op::EvalSexp, which calls `interp.eval_spanned` with
        // the INTERPRETER's environment. VM locals live in the value stack
        // at `locals_base` and are invisible to it — so any fallback form
        // referring to a local resolves to `unbound symbol` on the VM while
        // the tree-walker answers correctly.
        //
        // Zero of the pre-existing 58 cases exercised a fallback form with a
        // local in scope, which is why the whole class was green.
        ParityCase {
            name: "cond-reading-a-local",
            src: "(let ((x 7)) (cond ((> x 3) :big) (else :small)))",
        },
        ParityCase {
            name: "when-reading-a-local",
            src: "(let ((x 1)) (when (= x 1) :yes))",
        },
        ParityCase {
            name: "unless-reading-a-local",
            src: "(let ((x 2)) (unless (= x 1) :not-one))",
        },
        ParityCase {
            name: "let-star-reading-an-outer-local",
            src: "(let ((a 1)) (let* ((b (+ a 1)) (c (+ b 1))) c))",
        },
        ParityCase {
            name: "letrec-reading-an-outer-local",
            src: "(let ((n 3)) (letrec ((f (lambda (k) (if (= k 0) n (f (- k 1)))))) (f 2)))",
        },
        ParityCase {
            name: "cond-inside-a-lambda-body",
            src: "(define (classify v) (cond ((< v 0) :neg) ((= v 0) :zero) (else :pos))) (classify 5)",
        },
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
            name: "hof-callback-reads-a-capture",
            src: "(let ((k 10)) (map (lambda (x) (+ x k)) (list 1 2 3)))",
        },
        ParityCase {
            name: "hof-callback-sets-a-captured-cell",
            src: "(let ((n 0)) (for-each (lambda (x) (set! n (+ n x))) (list 1 2 3 4)) n)",
        },
        ParityCase {
            name: "hof-callback-sets-a-cell-another-closure-reads",
            src: "(let ((n 0))
                    (let ((read (lambda () n)))
                      (map (lambda (x) (set! n (+ n x)) (read)) (list 1 2 3))))",
        },
        ParityCase {
            name: "hof-nested-in-a-callback",
            src: "(let ((k 2))
                    (map (lambda (xs) (foldl (lambda (a x) (+ a (* k x))) 0 xs))
                         (list (list 1 2) (list 3 4 5))))",
        },
        ParityCase {
            name: "hof-callback-makes-a-closure",
            src: "(map (lambda (x) ((lambda (y) (+ x y)) 1)) (list 1 2))",
        },
        ParityCase {
            name: "hof-callback-calls-a-recursive-global",
            src: "(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))
                  (map (lambda (n) (fact n)) (list 3 4 5))",
        },
        ParityCase {
            name: "hof-callback-recurses-through-the-hof",
            src: "(define (depth t) (if (list? t) (+ 1 (foldl (lambda (a c) (max a (depth c))) 0 t)) 0))
                  (depth (list 1 (list 2 (list 3 (list 4)))))",
        },
        ParityCase {
            name: "hof-callback-raise-caught-outside",
            src: "(try (map (lambda (x) (if (= x 2) (throw (ex-info \"boom\" (list))) x)) (list 1 2 3))
                    (catch (e) (error-message e)))",
        },
        ParityCase {
            name: "hof-callback-runtime-error-caught-outside",
            src: "(try (map (lambda (x) (/ 1 x)) (list 1 0)) (catch (e) (error-tag e)))",
        },
        ParityCase {
            name: "hof-callback-unbound-caught-outside",
            src: "(try (map (lambda (x) (+ x nowhere)) (list 1)) (catch (e) (error-tag e)))",
        },
        ParityCase {
            name: "hof-callback-arity-caught-outside",
            src: "(try (map (lambda (a b) a) (list 1)) (catch (e) (error-tag e)))",
        },
        ParityCase {
            name: "hof-callback-catches-its-own-error",
            src: "(map (lambda (x) (try (/ 10 x) (catch (e) :inf))) (list 1 0 5))",
        },
        ParityCase {
            name: "hof-callback-with-eval-sexp-sees-its-locals",
            src: "(map (lambda (x) (force (delay (* x 2)))) (list 1 2 3))",
        },
        ParityCase {
            name: "hof-callback-rest-params",
            src: "(map (lambda (x &rest more) (list x more)) (list 1 2) (list 3 4))",
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
        // ── Tree-walker fallback (eval, macroexpand) ──────────────
        ParityCase {
            name: "fallback-eval-quoted",
            src: "(eval (quote (+ 1 2 3)))",
        },
        // ── Quasi-quote (native compilation, Phase 7) ─────────────
        ParityCase {
            name: "quasi-quote-with-global",
            src: "(define x 99) `(a ,x c)",
        },
        ParityCase {
            name: "quasi-quote-with-let-local",
            src: "(let ((x 42)) `(a ,x c))",
        },
        ParityCase {
            name: "quasi-quote-arithmetic-unquote",
            src: "(let ((y 10)) `(sum is ,(+ y 5)))",
        },
        ParityCase {
            name: "quasi-quote-unquote-splice",
            src: "(let ((xs (list 2 3 4))) `(1 ,@xs 5))",
        },
        ParityCase {
            name: "quasi-quote-mixed-splice",
            src: "(let ((a 1) (bs (list 2 3)) (c 4)) `(start ,a ,@bs ,c end))",
        },
        ParityCase {
            name: "quasi-quote-empty-list-result",
            src: "`()",
        },
        ParityCase {
            name: "quasi-quote-keyword-and-string",
            src: "`(:tag \"hello\" 42)",
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
            panic!("VM parity failures in {} cases:\n{msg}", PARITY_CASES.len());
        }
    }

    fn lifts_and_vms(src: &str) -> (Value, usize, usize) {
        let lifts = crate::vm::run::LIFTS.with(std::cell::Cell::get);
        let vms = crate::vm::run::CALLBACK_VMS.with(std::cell::Cell::get);
        let value = eval_vm(src, src);
        (
            value,
            crate::vm::run::LIFTS.with(std::cell::Cell::get) - lifts,
            crate::vm::run::CALLBACK_VMS.with(std::cell::Cell::get) - vms,
        )
    }

    #[test]
    fn a_hof_callback_runs_on_one_reused_vm_and_never_lifts() {
        let (value, lifts, vms) =
            lifts_and_vms("(map (lambda (x) (* x 3)) (list 1 2 3 4 5 6 7 8))");
        assert_eq!(format!("{value}"), "(3 6 9 12 15 18 21 24)");
        assert_eq!(lifts, 0, "a compiled callback went through lift_to_closure");
        assert_eq!(vms, 1, "one VM serves every call of one traversal");
    }

    #[test]
    fn each_traversal_gets_its_own_vm_and_nested_ones_do_too() {
        let (_, lifts, vms) = lifts_and_vms(
            "(map (lambda (xs) (map (lambda (x) (+ x 1)) xs)) (list (list 1 2) (list 3)))",
        );
        assert_eq!(lifts, 0);
        assert_eq!(vms, 3, "the outer map and each inner map own one VM");
    }

    #[test]
    fn a_callback_reaching_eval_sexp_keeps_the_lift() {
        let (value, lifts, _) =
            lifts_and_vms("(map (lambda (x) (force (delay (* x 2)))) (list 1 2 3))");
        assert_eq!(format!("{value}"), "(2 4 6)");
        assert_eq!(lifts, 3, "an EvalSexp body is lifted on every call");
    }

    #[test]
    fn a_callback_whose_inner_lambda_reaches_eval_sexp_keeps_the_lift() {
        let (_, lifts, _) =
            lifts_and_vms("(map (lambda (x) ((lambda () (force (delay x))))) (list 1 2))");
        assert_eq!(lifts, 2);
    }

    struct Quiet;

    impl crate::observe::CallObserver for Quiet {
        fn enter(&self, _: &crate::observe::Call<'_>, _: &[Value]) -> Option<Value> {
            None
        }
        fn leave(&self, _: &crate::observe::Call<'_>, _: crate::observe::Exit<'_>) {}
    }

    #[test]
    fn an_observed_interpreter_keeps_the_lift() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        i.set_call_observer(Some(std::sync::Arc::new(Quiet)));
        let before = crate::vm::run::LIFTS.with(std::cell::Cell::get);
        let forms = read_spanned("(map (lambda (x) x) (list 1 2))").unwrap();
        i.eval_program_vm(&forms, &mut NoHost).unwrap();
        assert_eq!(crate::vm::run::LIFTS.with(std::cell::Cell::get) - before, 2);
    }

    fn with_never_ready(src: &str, vm: bool) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        i.register_awaitable_fn(
            "never-ready",
            crate::ffi::Arity::Exact(0),
            |_: &[Value], _: &NoHost| false,
            |_: &[Value], _: &mut NoHost, _| Ok(Value::Nil),
        );
        let forms = read_spanned(src).unwrap();
        if vm {
            i.eval_program_vm(&forms, &mut NoHost)
        } else {
            i.eval_program(&forms, &mut NoHost)
        }
        .unwrap_or_else(|e| panic!("{src} on vm={vm}: {e:?}"))
    }

    #[test]
    fn a_park_inside_a_callback_answers_what_the_walker_answers() {
        let src = "(let ((n 0))
                     (list (length (map (lambda (x) (set! n (+ n 1)) (never-ready) x) (list 1 2 3))) n))";
        let tree = with_never_ready(src, false);
        let vm = with_never_ready(src, true);
        assert_eq!(format!("{tree}"), "(3 3)");
        assert_eq!(format!("{vm}"), format!("{tree}"));
    }

    #[test]
    fn callback_instructions_spend_the_interpreters_fuel() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        i.set_budget(crate::vm::Budget {
            fuel: Some(5_000),
            max_depth: Some(crate::vm::DEFAULT_MAX_DEPTH),
            quantum: None,
        })
        .unwrap();
        i.define_global(
            "xs",
            Value::list((0..10_000).map(Value::Int).collect::<Vec<_>>()),
        );
        let forms = read_spanned("(for-each (lambda (x) (+ x 1)) xs)").unwrap();
        let err = i.eval_program_vm(&forms, &mut NoHost).unwrap_err();
        assert_eq!(err.tag(), "fuel-exhausted", "{err:?}");
    }

    #[test]
    fn the_depth_bound_holds_across_nested_callbacks() {
        let src = "(define (deep n) (if (= n 0) 0 (car (map (lambda (x) (+ 1 (deep (- n 1)))) (list n)))))
                   (deep 200)";
        for vm in [false, true] {
            let mut i: Interpreter<NoHost> = Interpreter::new();
            install_full_stdlib_with(&mut i, &mut NoHost);
            i.set_budget(crate::vm::Budget {
                fuel: None,
                max_depth: Some(100),
                quantum: None,
            })
            .unwrap();
            let forms = read_spanned(src).unwrap();
            let out = if vm {
                i.eval_program_vm(&forms, &mut NoHost)
            } else {
                i.eval_program(&forms, &mut NoHost)
            };
            assert_eq!(
                out.map_err(|e| e.tag()).unwrap_err(),
                "depth-exceeded",
                "vm={vm}"
            );
        }
    }

    #[test]
    fn deep_recursion_through_callbacks_matches_the_walker() {
        let src = "(define (deep n) (if (= n 0) 0 (car (map (lambda (x) (+ 1 (deep (- n 1)))) (list n)))))
                   (deep 3000)";
        assert_eq!(format!("{}", eval_vm(src, src)), "3000");
        assert_eq!(format!("{}", eval_tree(src, src)), "3000");
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
