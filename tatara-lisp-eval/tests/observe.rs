//! The call hook's two arms: the observer a host installs, and the unwind
//! record every error leaves.
//!
//! Red runs, recorded 2026-10-10:
//!
//! - with the `observer.enter` call removed from `call_closure_body`,
//!   `the_observer_sees_every_activation_tail_calls_included` fails
//!   (`left: 0, right: 11`) and `enters_and_leaves_balance_on_every_path` with it;
//! - with `registry.unwind.push` removed, `an_error_names_every_frame_it_left_innermost_first`,
//!   `a_caught_error_leaves_no_trace` and the deep refusal fail on an empty
//!   trace;
//! - with the two `unwind.clear()` calls in `sf_try` removed,
//!   `a_caught_error_leaves_no_trace` fails: the caught error's `inner` frame
//!   is still there beside the reported one.

use std::sync::{Arc, Mutex};

use tatara_lisp_eval::observe::{Call, CallObserver, Exit, TRACE_FRAMES};
use tatara_lisp_eval::vm::Budget;
use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, EvalError, Interpreter, Value};

fn interpreter() -> Interpreter<()> {
    let mut interp = Interpreter::new();
    install_full_stdlib_with(&mut interp, &mut ());
    interp
}

fn run(interp: &mut Interpreter<()>, src: &str) -> Result<Value, EvalError> {
    let forms = read_spanned(src).expect("test source reads");
    interp.eval_program(&forms, &mut ())
}

/// Every event, as `+name` on enter and `-name:how` on leave.
#[derive(Default)]
struct Log(Mutex<Vec<String>>);

impl Log {
    fn events(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

fn name(call: &Call<'_>) -> String {
    call.name.map_or_else(|| "λ".to_string(), |n| n.to_string())
}

impl CallObserver for Log {
    fn enter(&self, call: &Call<'_>, args: &[Value]) -> Option<Value> {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        self.0
            .lock()
            .unwrap()
            .push(format!("+{}({})", name(call), args.join(",")));
        None
    }
    fn leave(&self, call: &Call<'_>, exit: Exit<'_>) {
        let how = match exit {
            Exit::Returned(v) => format!("={v}"),
            Exit::TailCalled => "tail".to_string(),
            Exit::Raised(_) => "raised".to_string(),
            Exit::Supplied(v) => format!("supplied {v}"),
        };
        self.0
            .lock()
            .unwrap()
            .push(format!("-{}:{how}", name(call)));
    }
}

fn watched() -> (Interpreter<()>, Arc<Log>) {
    let mut interp = interpreter();
    let log = Arc::new(Log::default());
    interp.set_call_observer(Some(log.clone()));
    (interp, log)
}

#[test]
fn the_observer_sees_every_activation_tail_calls_included() {
    let (mut interp, log) = watched();
    run(
        &mut interp,
        "(define (down n) (if (= n 0) :done (down (- n 1)))) (down 10)",
    )
    .unwrap();
    let enters = log
        .events()
        .iter()
        .filter(|e| e.starts_with("+down"))
        .count();
    assert_eq!(enters, 11);
    let events = log.events();
    assert_eq!(events[0], "+down(10)");
    assert_eq!(events[1], "-down:tail");
    assert_eq!(events.last().unwrap(), "-down:=:done");
}

#[test]
fn enters_and_leaves_balance_on_every_path() {
    let (mut interp, log) = watched();
    let r = run(
        &mut interp,
        "(define (boom) (car (list)))
         (define (mid) (+ 1 (boom)))
         (define (safe) (try (mid) (catch (e) 7)))
         (define (twice x) (* 2 x))
         (+ (safe) (car (map (lambda (x) (twice x)) (list 1 2))))",
    )
    .unwrap();
    assert!(matches!(r, Value::Int(9)), "{r:?}");
    let events = log.events();
    let enters = events.iter().filter(|e| e.starts_with('+')).count();
    let leaves = events.iter().filter(|e| e.starts_with('-')).count();
    assert_eq!(enters, leaves, "{events:?}");
    assert!(events.contains(&"-boom:raised".to_string()), "{events:?}");
    assert!(events.contains(&"-mid:raised".to_string()), "{events:?}");
    assert!(events.contains(&"-safe:=7".to_string()), "{events:?}");
    assert!(events.contains(&"+λ(1)".to_string()), "{events:?}");
}

#[test]
fn a_fork_is_watched_by_its_parents_observer() {
    let (mut interp, log) = watched();
    run(&mut interp, "(define (one) 1)").unwrap();
    let mut child = interp.fork();
    run(&mut child, "(one)").unwrap();
    assert_eq!(log.events(), vec!["+one()", "-one:=1"]);
}

#[test]
fn an_error_names_every_frame_it_left_innermost_first() {
    let mut interp = interpreter();
    let err = run(
        &mut interp,
        "(define (inner xs) (car xs))
         (define (middle) (+ 1 (inner (list))))
         (define (outer) (* 2 (middle)))
         (outer)",
    )
    .unwrap_err();
    assert!(err.to_string().contains("car"), "{err}");
    let trace = interp.take_trace();
    let names: Vec<_> = trace
        .frames
        .iter()
        .map(|f| f.name.as_deref().unwrap_or("λ").to_string())
        .collect();
    assert_eq!(names, ["inner", "middle", "outer"]);
    assert_eq!(trace.depth, 3);
    assert!(interp.take_trace().frames.is_empty(), "take empties it");
}

#[test]
fn a_caught_error_leaves_no_trace() {
    let mut interp = interpreter();
    let err = run(
        &mut interp,
        "(define (inner) (car (list)))
         (define (caught) (try (inner) (catch (e) 0)))
         (define (later) (+ (caught) (car (list))))
         (later)",
    )
    .unwrap_err();
    assert!(err.to_string().contains("car"), "{err}");
    let names: Vec<_> = interp
        .take_trace()
        .frames
        .iter()
        .map(|f| f.name.as_deref().unwrap_or("λ").to_string())
        .collect();
    assert_eq!(names, ["later"]);
}

#[test]
fn a_deep_refusal_keeps_a_bounded_trace_and_counts_the_rest() {
    let mut interp = interpreter();
    interp
        .set_budget(Budget {
            fuel: None,
            max_depth: Some(5_000),
            quantum: None,
        })
        .unwrap();
    let err = run(&mut interp, "(define (f n) (+ 1 (f (+ n 1)))) (f 0)").unwrap_err();
    assert!(matches!(err, EvalError::BudgetExceeded { .. }), "{err}");
    let trace = interp.take_trace();
    assert_eq!(trace.frames.len(), TRACE_FRAMES);
    assert_eq!(trace.depth, 5_000);
}

#[test]
fn each_top_level_form_starts_with_an_empty_trace() {
    let mut interp = interpreter();
    let _ = run(&mut interp, "(define (f) (car (list))) (f)").unwrap_err();
    run(&mut interp, "(+ 1 2)").unwrap();
    assert!(interp.take_trace().frames.is_empty());
}

/// Answers every call of `slow` with 42 and counts how often its body ran.
#[derive(Default)]
struct Memo {
    ran: Mutex<usize>,
}

impl CallObserver for Memo {
    fn enter(&self, call: &Call<'_>, _args: &[Value]) -> Option<Value> {
        (call.name.map(|n| &**n) == Some("slow")).then_some(Value::Int(42))
    }
    fn leave(&self, call: &Call<'_>, exit: Exit<'_>) {
        if call.name.map(|n| &**n) == Some("slow") && !matches!(exit, Exit::Supplied(_)) {
            *self.ran.lock().unwrap() += 1;
        }
    }
}

/// An observer can answer a call without its body running.
///
/// Red run (2026-10-10): the `Some(v)` arm in `call_closure_body` ignored, so
/// the body always runs: the run raises on `car` of an empty list instead of
/// answering 42.
#[test]
fn an_observer_can_supply_a_calls_value() {
    let mut interp = interpreter();
    let memo = Arc::new(Memo::default());
    interp.set_call_observer(Some(memo.clone()));
    let v = run(&mut interp, "(define (slow n) (car (list))) (+ 0 (slow 1))").unwrap();
    assert!(matches!(v, Value::Int(42)), "{v:?}");
    assert_eq!(*memo.ran.lock().unwrap(), 0);
}
