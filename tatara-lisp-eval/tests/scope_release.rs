//! A scope that ends frees its frame, even when a closure it bound captured it.
//!
//! `interpreter_release.rs` gates the ring at the scale of a whole interpreter:
//! a dead fork frees its own frame. This file gates it at the scale that runs
//! millions of times: one function call, one `let*`, one `letrec`. A function
//! whose body binds a local lambda — blue's `add = fn(x) x + i end`, lowered to
//! `(define add (lambda …))` — builds Frame → Closure → Env → Frame on EVERY
//! call, and before scope release nothing ever broke it. Measured in blue
//! (release, `/usr/bin/time -l`): 100 000 calls held 116 MB, 400 000 held
//! 430 MB, about 1 KB per call, while the same loop with an unbound block
//! stayed at 11 MB.
//!
//! The probe is a `Value::Foreign` sentinel made inside the scope by a native
//! `(sentinel)`: it is freed exactly when the frame that binds it is, so the
//! drop counter reports frames without reaching into the allocator.
//!
//! ## Red run, recorded (2026-10-10, tatara-lisp-eval 0.4.1)
//!
//! Against the tree before `Env::release_scope` existed:
//!
//! ```text
//! test a_body_define_of_a_lambda_frees_the_call_frame ... FAILED
//! test a_letrec_loop_in_tail_position_frees_its_frame ... FAILED
//! test a_let_star_lambda_frees_the_let_frame ... FAILED
//! test a_parameter_aliasing_a_global_function_does_not_block_release ... FAILED
//! test an_error_unwinding_through_the_scope_frees_it ... FAILED
//! test the_control_frees_every_frame ... ok
//! test an_escaped_closure_keeps_its_frame_and_still_runs ... ok
//! test a_shared_list_tail_does_not_count_one_closure_twice ... FAILED
//! ```
//!
//! The last arm is a soundness gate, not a leak gate: with persistent lists two
//! lists share their nodes, so one closure is reachable from both while its own
//! `Arc` count stays 1. Walking into lists counted it twice, the exclusivity
//! equation balanced against an escaped closure's handle, and the frame was
//! cleared under it: `UnboundSymbol { name: "a" }`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tatara_lisp_eval::ffi::Arity;
use tatara_lisp_eval::{install_full_stdlib_with, Interpreter, Span, Value};

struct Sentinel(Arc<AtomicUsize>);

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// An interpreter with the full stdlib and a `(sentinel)` that makes a fresh
/// probe each call, plus the counter its drops land in.
fn probed() -> (Interpreter<()>, Arc<AtomicUsize>) {
    let freed = Arc::new(AtomicUsize::new(0));
    let mut interp = Interpreter::new();
    install_full_stdlib_with(&mut interp, &mut ());
    let counter = freed.clone();
    interp.register_fn(
        "sentinel",
        Arity::Exact(0),
        move |_: &[Value], _: &mut (), _: Span| {
            Ok(Value::Foreign(Arc::new(Sentinel(counter.clone()))))
        },
    );
    (interp, freed)
}

fn eval(interp: &mut Interpreter<()>, src: &str) -> Value {
    let forms = tatara_lisp_eval::read_spanned(src).expect("parse");
    interp
        .eval_program(&forms, &mut ())
        .unwrap_or_else(|e| panic!("{src}\n  failed: {e}"))
}

/// Runs `step` `CALLS` times from a loop that holds nothing, then reports how
/// many sentinels were freed while the interpreter is still alive.
const CALLS: usize = 50;

fn freed_after_calls(defs: &str, call: &str) -> usize {
    let (mut interp, freed) = probed();
    eval(&mut interp, defs);
    eval(
        &mut interp,
        &format!("(define (drive n) (if (= n 0) 0 (begin {call} (drive (- n 1)))))"),
    );
    eval(&mut interp, &format!("(drive {CALLS})"));
    freed.load(Ordering::SeqCst)
}

// ── the control ────────────────────────────────────────────────────────────

/// **CONTROL.** The same call with the lambda passed straight to a
/// higher-order function and never bound, so no ring exists. It frees every
/// frame before the fix and after it; without it the arms below could be
/// reading a sentinel that is never dropped for an unrelated reason.
#[test]
fn the_control_frees_every_frame() {
    let n = freed_after_calls(
        "(define (step) (define s (sentinel)) (map (lambda (x) x) (list 1)))",
        "(step)",
    );
    assert_eq!(n, CALLS, "the control must observe every frame being freed");
}

// ── the leak arms ──────────────────────────────────────────────────────────

/// blue's local function: `(define add (lambda …))` in a function body.
#[test]
fn a_body_define_of_a_lambda_frees_the_call_frame() {
    let n = freed_after_calls(
        "(define (step i) (define s (sentinel)) (define add (lambda (x) (+ x i))) (add 1))",
        "(step 1)",
    );
    assert_eq!(
        n, CALLS,
        "every call's frame bound a lambda that captured it; each must be freed when the \
         call returns"
    );
}

/// `let*` evaluates each right-hand side inside the new frame, so a lambda
/// there captures the frame that binds it.
#[test]
fn a_let_star_lambda_frees_the_let_frame() {
    let n = freed_after_calls(
        "(define (step) (+ 1 (let* ((s (sentinel)) (f (lambda (x) x))) (f 1))))",
        "(step)",
    );
    assert_eq!(n, CALLS, "the let* frame must be freed when the let* ends");
}

/// The loop idiom: a `letrec` in tail position hands its own closure to the
/// trampoline, so the frame is still held when the `letrec` pops it. It must be
/// freed when the trampoline lets go of the closure.
#[test]
fn a_letrec_loop_in_tail_position_frees_its_frame() {
    let n = freed_after_calls(
        "(define (step) \
           (letrec ((s (sentinel)) \
                    (lp (lambda (k) (if (= k 0) 0 (lp (- k 1)))))) \
             (lp 3)))",
        "(step)",
    );
    assert_eq!(
        n, CALLS,
        "the letrec frame must be freed once its loop returns"
    );
}

/// A parameter bound to a global function is an aliased closure in the frame.
/// It holds no handle to the frame, so it must not stop the frame's release.
#[test]
fn a_parameter_aliasing_a_global_function_does_not_block_release() {
    let n = freed_after_calls(
        "(define (twice x) (* 2 x)) \
         (define (step f) (define s (sentinel)) (define (g x) (f x)) (g 1))",
        "(step twice)",
    );
    assert_eq!(
        n, CALLS,
        "an aliased closure that holds no handle must count zero"
    );
}

/// A `throw` unwinding through the call, caught one level up.
#[test]
fn an_error_unwinding_through_the_scope_frees_it() {
    let n = freed_after_calls(
        "(define (step) (define s (sentinel)) (define (g) (throw (ex-info \"no\" {}))) (g))",
        "(try (step) (catch (e) 0))",
    );
    assert_eq!(
        n, CALLS,
        "a frame left by an error must be freed like one left by a return"
    );
}

// ── the soundness arms ─────────────────────────────────────────────────────

/// A closure returned out of its scope still resolves names in that scope's
/// frame, so the frame must NOT be released while the closure lives.
#[test]
fn an_escaped_closure_keeps_its_frame_and_still_runs() {
    let (mut interp, freed) = probed();
    eval(
        &mut interp,
        "(define (make) (define s (sentinel)) (define (count n) (if (= n 0) s (count (- n 1)))) count)",
    );
    eval(&mut interp, "(define kept (make))");
    assert_eq!(
        freed.load(Ordering::SeqCst),
        0,
        "the escaped closure still needs the frame"
    );
    let got = eval(&mut interp, "(kept 5)");
    assert!(
        matches!(got, Value::Foreign(_)),
        "the escaped closure resolves its own name and `s` through its frame; got {got:?}"
    );
}

/// Two persistent lists that share structure hold ONE closure value between
/// them. It must not be counted once per list.
#[test]
fn a_shared_list_tail_does_not_count_one_closure_twice() {
    let (mut interp, _freed) = probed();
    let mut child = interp.fork();
    let escaped = eval(
        &mut child,
        "(define a (cons (lambda () 0) (range 0 200))) \
         (define b (append a (list 1))) \
         (lambda () (length a))",
    );
    drop(child);
    let got = interp
        .apply_external_value(&escaped, vec![], &mut (), Span::synthetic())
        .expect("the escaped closure must still read `a`");
    assert!(matches!(got, Value::Int(201)), "got {got:?}");
}
