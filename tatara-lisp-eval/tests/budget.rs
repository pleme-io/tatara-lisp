//! The tree-walker's execution budget: depth and fuel.
//!
//! Measured before any of this existed (tatara-script 0.3.62): a non-tail
//! recursion 6,000 deep aborted the process — `fatal runtime error: stack
//! overflow, aborting`, rc 134 — and a `try` wrapped around it could not see
//! it, because the OS process was already gone. A tail loop with no exit ran
//! until killed. Every test here is a property that was false then.
//!
//! Red runs, recorded 2026-09-29:
//!
//! - with `stacker::maybe_grow` removed from `eval::call_closure`, the test
//!   binary itself aborts in `a_recursion_far_past_the_thread_stack_completes`
//!   (`has overflowed its stack`), which is the old behaviour exactly;
//! - with the `Meter::enter` call removed, `depth_past_the_bound_is_refused_and_names_the_function`
//!   fails (the recursion completes);
//! - with the fuel comparison in `FnRegistry::step` removed,
//!   `a_loop_with_no_exit_runs_out_of_fuel_in_the_function_that_spins` spins
//!   until its 20 s watchdog fails it — the watchdog is why the fuel tests run
//!   on their own thread rather than hanging the suite.

use std::sync::mpsc;
use std::time::Duration;

use tatara_lisp_eval::error::BudgetDimension;
use tatara_lisp_eval::vm::{Budget, DEFAULT_MAX_DEPTH};
use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, EvalError, Interpreter, Value};

const DEEP: &str = "(define (f n) (if (= n 0) 0 (+ 1 (f (- n 1)))))";
const SPIN: &str = "(define (spin n) (spin (+ n 1)))";

fn interpreter() -> Interpreter<()> {
    let mut interp = Interpreter::new();
    install_full_stdlib_with(&mut interp, &mut ());
    interp
}

fn run(interp: &mut Interpreter<()>, src: &str) -> Result<Value, EvalError> {
    let forms = read_spanned(src).expect("test source reads");
    interp.eval_program(&forms, &mut ())
}

/// Run `f` on its own thread and fail the test, rather than hang the suite,
/// if it has not finished in `secs`. The fuel tests need this: their red run
/// is an infinite loop.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .expect("did not finish: the budget did not stop the run")
}

fn budget(fuel: Option<usize>, max_depth: Option<usize>) -> Budget {
    Budget {
        fuel,
        max_depth,
        quantum: None,
    }
}

#[test]
fn the_default_bounds_depth_and_leaves_fuel_to_the_host() {
    let b = interpreter().budget();
    assert_eq!(b.max_depth, Some(DEFAULT_MAX_DEPTH));
    assert_eq!(b.fuel, None);
    assert_eq!(DEFAULT_MAX_DEPTH, 100_000, "the shipped value, pinned");
}

/// The recursion that aborted the process at 6,000 now completes at ten
/// times that, on the default budget.
#[test]
fn a_recursion_far_past_the_thread_stack_completes() {
    let mut i = interpreter();
    let v = run(&mut i, &format!("{DEEP} (f 60000)")).expect("60k frames evaluate");
    assert!(matches!(v, Value::Int(60_000)), "{v:?}");
}

/// Stack growth is what makes the depth bound independent of the thread: the
/// same recursion completes on a thread whose stack is 256 KiB, a size at
/// which the pre-growth evaluator overflowed before 200 frames.
#[test]
fn depth_does_not_depend_on_the_thread_stack() {
    let v = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut i = interpreter();
            run(&mut i, &format!("{DEEP} (f 20000)")).map(|v| format!("{v}"))
        })
        .unwrap()
        .join()
        .expect("no overflow on a small thread");
    assert_eq!(v.unwrap(), "20000");
}

#[test]
fn depth_past_the_bound_is_refused_and_names_the_function() {
    let mut i = interpreter();
    i.set_budget(budget(None, Some(500))).unwrap();
    let err = run(&mut i, &format!("{DEEP} (f 1000)")).unwrap_err();
    match &err {
        EvalError::BudgetExceeded {
            dimension: BudgetDimension::Depth,
            limit: 500,
            function: Some(f),
            ..
        } => assert_eq!(&**f, "f"),
        other => panic!("expected a depth refusal naming `f`, got {other:?}"),
    }
    assert_eq!(err.tag(), "depth-exceeded");
    let msg = err.to_string();
    assert!(msg.contains("500") && msg.contains("`f`"), "{msg}");
}

/// Exactly at the bound passes; one past it is refused. `(f n)` holds n+1
/// closure frames (n down to 0), so the edge is n = max_depth - 1.
#[test]
fn the_depth_bound_is_exact() {
    let mut i = interpreter();
    i.set_budget(budget(None, Some(100))).unwrap();
    assert!(run(&mut i, &format!("{DEEP} (f 99)")).is_ok());
    assert!(matches!(
        run(&mut i, "(f 100)"),
        Err(EvalError::BudgetExceeded {
            dimension: BudgetDimension::Depth,
            ..
        })
    ));
}

/// A depth refusal is catchable, and the handler runs: the frames it counted
/// have unwound by the time `catch` sees it, and the interpreter is usable
/// afterwards (the guard released every frame on the error path).
#[test]
fn try_catches_a_depth_refusal_and_the_interpreter_recovers() {
    let mut i = interpreter();
    i.set_budget(budget(None, Some(1000))).unwrap();
    let v = run(
        &mut i,
        &format!("{DEEP} (try (f 5000) (catch (e) (error-tag e)))"),
    )
    .expect("catch sees the refusal");
    assert_eq!(format!("{v}"), ":depth-exceeded");
    // Every frame was left: 999 frames still fit.
    let v = run(&mut i, "(f 998)").expect("depth was fully released");
    assert!(matches!(v, Value::Int(998)), "{v:?}");
}

/// The default bound is a catchable refusal too — this is the exact probe the
/// gap ledger recorded as an uncatchable abort (`try(f(100000), …)`).
#[test]
fn the_ledger_probe_is_caught_on_the_default_budget() {
    let mut i = interpreter();
    let v = run(
        &mut i,
        &format!("{DEEP} (try (f 150000) (catch (e) (error-tag e)))"),
    )
    .expect("caught");
    assert_eq!(format!("{v}"), ":depth-exceeded");
}

/// Tail calls reuse the frame, so they count against neither bound's depth.
#[test]
fn tail_calls_do_not_count_against_depth() {
    let mut i = interpreter();
    i.set_budget(budget(None, Some(10))).unwrap();
    let v = run(
        &mut i,
        "(define (loop n) (if (= n 0) :done (loop (- n 1)))) (loop 200000)",
    )
    .expect("a tail loop fits in any depth");
    assert_eq!(format!("{v}"), ":done");
}

#[test]
fn a_loop_with_no_exit_runs_out_of_fuel_in_the_function_that_spins() {
    let err = within(20, || {
        let mut i = interpreter();
        i.set_budget(budget(Some(100_000), None)).unwrap();
        run(&mut i, &format!("{SPIN} (spin 0)")).unwrap_err()
    });
    match &err {
        EvalError::BudgetExceeded {
            dimension: BudgetDimension::Fuel,
            limit: 100_000,
            function: Some(f),
            ..
        } => assert_eq!(&**f, "spin"),
        other => panic!("expected fuel exhaustion in `spin`, got {other:?}"),
    }
    assert_eq!(err.tag(), "fuel-exhausted");
}

/// A catch observes exhausted fuel but cannot spend past it: the handler's
/// first step refuses again, so the error leaves the `try`.
#[test]
fn fuel_cannot_be_caught_and_spent_past() {
    let err = within(20, || {
        let mut i = interpreter();
        i.set_budget(budget(Some(50_000), None)).unwrap();
        run(&mut i, &format!("{SPIN} (try (spin 0) (catch (e) 42))")).unwrap_err()
    });
    assert_eq!(err.tag(), "fuel-exhausted", "{err}");
}

/// Steps are counted whether or not fuel is bounded, and a terminating
/// program under a budget above its cost is unaffected — the bound changes
/// whether a runaway is refused, never a value.
#[test]
fn fuel_above_the_cost_changes_nothing() {
    let mut i = interpreter();
    // Setting a budget starts the count, so the stdlib's own evaluation at
    // install time is not in it.
    i.set_budget(Budget::tree_walker_default()).unwrap();
    let v = run(&mut i, &format!("{DEEP} (f 1000)")).unwrap();
    let cost = i.steps_spent();
    assert!(
        cost > 1000,
        "steps are counted on the default budget: {cost}"
    );

    let mut j = interpreter();
    j.set_budget(budget(Some(cost), None)).unwrap();
    let w = run(&mut j, &format!("{DEEP} (f 1000)")).expect("exactly enough fuel");
    assert_eq!(format!("{v}"), format!("{w}"));

    let mut k = interpreter();
    k.set_budget(budget(Some(cost - 1), None)).unwrap();
    assert_eq!(
        run(&mut k, &format!("{DEEP} (f 1000)")).unwrap_err().tag(),
        "fuel-exhausted",
        "one step short is refused"
    );
}

/// A fork is a new incarnation: it inherits the budget and gets all of it.
#[test]
fn a_fork_starts_with_the_whole_budget() {
    let mut parent = interpreter();
    run(&mut parent, SPIN).unwrap();
    parent.set_budget(budget(Some(5_000), Some(50))).unwrap();
    assert!(run(&mut parent, "(spin 0)").is_err());
    assert!(parent.steps_spent() > 5_000);

    let child = parent.fork();
    assert_eq!(child.budget().fuel, Some(5_000));
    assert_eq!(child.budget().max_depth, Some(50));
    assert_eq!(child.steps_spent(), 0);
}

#[test]
fn a_quantum_is_refused_rather_than_ignored() {
    let mut i = interpreter();
    assert!(i.set_budget(Budget::preemptive(10)).is_err());
    assert_eq!(i.budget().max_depth, Some(DEFAULT_MAX_DEPTH), "unchanged");
}

/// Higher-order primitives re-enter the evaluator through the same door, so a
/// recursion through `map` is bounded too.
#[test]
fn recursion_through_a_native_higher_order_fn_is_bounded() {
    let mut i = interpreter();
    i.set_budget(budget(None, Some(300))).unwrap();
    let err = run(
        &mut i,
        "(define (g n) (if (= n 0) 0 (car (map (lambda (x) (+ 1 (g (- n 1)))) (list 1))))) (g 1000)",
    )
    .unwrap_err();
    assert_eq!(err.tag(), "depth-exceeded", "{err}");
}

/// Both executors refuse the same program with the same error: one budget per
/// interpreter (`eval_program_vm` runs under `Interpreter::budget`), one
/// message, the same function named. blue's conformance suite compares the two
/// messages byte for byte.
#[test]
fn the_walker_and_the_vm_refuse_alike() {
    fn both(src: &str, b: Budget) -> (String, String) {
        let forms = read_spanned(src).unwrap();
        let mut w = interpreter();
        w.set_budget(b).unwrap();
        let walker = w.eval_program(&forms, &mut ()).unwrap_err();
        let mut v = interpreter();
        v.set_budget(b).unwrap();
        let vm = v.eval_program_vm(&forms, &mut ()).unwrap_err();
        assert_eq!(walker.tag(), vm.tag());
        (walker.short_message(), vm.short_message())
    }
    let (w, v) = both(&format!("{DEEP} (f 1000)"), budget(None, Some(100)));
    assert_eq!(w, v);
    assert_eq!(w, "call depth budget of 100 exceeded in `f`");
    let (w, v) = within(20, || {
        both(&format!("{SPIN} (spin 0)"), budget(Some(10_000), None))
    });
    assert_eq!(w, v);
    assert_eq!(w, "fuel budget of 10000 exceeded in `spin`");
}

/// And a `try` around each sees the same thing: depth caught on both.
#[test]
fn the_vm_catches_a_depth_refusal_like_the_walker() {
    let src = format!("{DEEP} (try (f 1000) (catch (e) (error-message e)))");
    let forms = read_spanned(&src).unwrap();
    let mut w = interpreter();
    w.set_budget(budget(None, Some(100))).unwrap();
    let mut v = interpreter();
    v.set_budget(budget(None, Some(100))).unwrap();
    let a = format!("{}", w.eval_program(&forms, &mut ()).unwrap());
    let b = format!("{}", v.eval_program_vm(&forms, &mut ()).unwrap());
    assert_eq!(a, b);
    assert_eq!(a, "\"call depth budget of 100 exceeded in `f`\"");
}
