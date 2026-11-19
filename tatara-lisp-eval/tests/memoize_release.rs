//! A closure that keeps a function argument dies with its interpreter.
//!
//! `(define (mk h) (lambda (x) (h x)))` called with a lambda: the returned
//! closure captured `mk`'s call frame, that frame binds `h`, and `h` captured
//! the globals that bind the returned closure. The globals' handle sits one
//! frame away from the closure that holds it, which the exclusivity count did
//! not look through, so every higher-order constructor (`memoize` among them,
//! 12-15 KB per blue run, measured 2026-11-19) outlived its interpreter.

use std::sync::{Arc, Weak};

use tatara_lisp_eval::value::Closure;
use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, Value};

fn made(src: &str, fork: bool) -> Weak<Closure> {
    let mut base: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut base, &mut ());
    let mut i = if fork { base.fork() } else { base };
    let forms = read_spanned(src).expect("reads");
    i.eval_program(&forms, &mut ()).expect("runs");
    let Some(Value::Closure(f)) = i.lookup_global("f") else {
        panic!("f is a closure");
    };
    let weak = Arc::downgrade(&f);
    drop(f);
    drop(i);
    weak
}

const SHAPES: [(&str, &str); 4] = [
    ("memoize", "(define f (memoize (lambda (x) x)))"),
    ("closure argument", "(define (mk h) (lambda (x) (h x))) (define f (mk (lambda (y) y)))"),
    (
        "memoize written out",
        "(define (mm g) (let ((cache (hash-map))) (lambda (&rest args) (let ((k (pr-str args))) (if (hash-map-has? cache k) (hash-map-get cache k) (apply g args)))))) (define f (mm (lambda (x) x)))",
    ),
    ("let over lambda", "(define (mk) (let ((c 1)) (lambda (x) c))) (define f (mk))"),
];

#[test]
fn a_closure_that_keeps_a_function_argument_dies_with_its_interpreter() {
    for (name, src) in SHAPES {
        for fork in [false, true] {
            assert!(
                made(src, fork).upgrade().is_none(),
                "{name} (fork {fork}) outlived its interpreter"
            );
        }
    }
}

#[test]
fn a_memoized_function_still_answers_while_its_interpreter_lives() {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    let forms = read_spanned(
        "(define (mk h) (lambda (x) (h x))) (define g (mk (lambda (y) (* y 2)))) (define f (memoize (lambda (x) (+ x 1)))) (list (g 4) (f 1) (f 1))",
    )
    .expect("reads");
    let got = i.eval_program(&forms, &mut ()).expect("runs");
    assert_eq!(format!("{got}"), "(8 2 2)");
}
