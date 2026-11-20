//! A `letrec` on the VM does not outlive its scope.
//!
//! The VM lowers `(letrec ((loop (lambda …))) …)` to a nil cell set
//! afterwards, and a closure captures by cell, so the loop lambda holds the
//! cell that holds it. When the frame went, the cell and the closure kept each
//! other alive with everything the lambda captured: blue's `while` (a letrec
//! loop) left 13 KB per run on the VM and nothing on the walker (blue
//! docs/pure-blue/front-tower-conformance.md, C2, measured 2026-11-20).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Arity, Interpreter, Value};

static DROPPED: AtomicUsize = AtomicUsize::new(0);

struct Probe;

impl Drop for Probe {
    fn drop(&mut self) {
        DROPPED.fetch_add(1, Ordering::SeqCst);
    }
}

fn run_vm(src: &str) -> String {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i.register_fn("probe", Arity::Exact(0), |_: &[Value], _: &mut (), _| {
        Ok(Value::Foreign(Arc::new(Probe)))
    });
    let got = i
        .eval_program_vm(&read_spanned(src).expect("reads"), &mut ())
        .expect("runs");
    format!("{got}")
}

#[test]
fn a_letrec_loop_and_what_it_captured_die_with_their_scope_on_the_vm() {
    let shapes = [
        (
            "a loop in a function",
            "(define (f) (let ((p (probe))) (letrec ((loop (lambda (n) (if (> n 0) (loop (- n 1)) p)))) (loop 2)) 0)) (f)",
        ),
        (
            "a loop at the top level",
            "(let ((p (probe))) (letrec ((loop (lambda (n) (if (> n 0) (loop (- n 1)) p)))) (loop 2)) 0)",
        ),
        (
            "while",
            "(define (f) (let ((p (probe)) (i 0)) (while (< i 2) (set! i (+ i 1)) p) i)) (f)",
        ),
    ];
    for (name, src) in shapes {
        let before = DROPPED.load(Ordering::SeqCst);
        run_vm(src);
        assert_eq!(
            DROPPED.load(Ordering::SeqCst),
            before + 1,
            "{name}: the probe the loop captured outlived the run"
        );
    }
}

#[test]
fn a_letrec_that_returns_its_function_keeps_it_working() {
    assert_eq!(
        run_vm("(define (mk) (letrec ((loop (lambda (n) (if (> n 0) (loop (- n 1)) n)))) loop)) ((mk) 3)"),
        "0"
    );
    assert_eq!(
        run_vm(
            "(define g (letrec ((loop (lambda (n) (if (> n 0) (loop (- n 1)) 7)))) loop)) (g 4)"
        ),
        "7"
    );
}
