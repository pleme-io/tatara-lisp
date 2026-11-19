//! A closure a host keeps inside a `Value::Foreign` container (a dispatch
//! table, a memo table) holds the frames it captured, and those frames bind
//! the container: a ring that `Env::release_own_frames` cannot see into, so
//! the interpreter dies and the ring outlives it. Measured in blue
//! (2026-11-19): `verb_for` with a closure handler leaves 11,647 bytes live per
//! run and `memoize` 14,787. `Interpreter::on_release` lets the host empty its
//! container first, so the ring falls with the interpreter.

use std::sync::{Arc, Mutex, Weak};

use tatara_lisp_eval::value::Closure;
use tatara_lisp_eval::{read_spanned, Interpreter, Value};

fn ring(hook: bool) -> Weak<Closure> {
    let mut i: Interpreter<()> = Interpreter::new();
    let forms = read_spanned("(define n 1) (define f (lambda () n))").expect("reads");
    i.eval_program(&forms, &mut ()).expect("defines");
    let Some(Value::Closure(f)) = i.lookup_global("f") else {
        panic!("f is a closure");
    };
    let weak = Arc::downgrade(&f);
    let holder: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![Value::Closure(f)]));
    i.define_global("holder", Value::Foreign(holder.clone()));
    if hook {
        let held = holder.clone();
        i.on_release(move || held.lock().unwrap_or_else(|p| p.into_inner()).clear());
    }
    drop(holder);
    drop(i);
    weak
}

#[test]
fn a_closure_held_in_a_foreign_container_outlives_its_interpreter_without_a_hook() {
    assert!(
        ring(false).upgrade().is_some(),
        "without a hook the container keeps the closure, and the closure keeps the frames that bind the container"
    );
}

#[test]
fn a_release_hook_that_empties_the_container_frees_the_ring() {
    assert!(
        ring(true).upgrade().is_none(),
        "the hook empties the container before the frames are released, so nothing holds the closure"
    );
}

#[test]
fn a_fork_does_not_run_its_parents_hooks() {
    let held: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![Value::Int(1)]));
    let mut parent: Interpreter<()> = Interpreter::new();
    let h = held.clone();
    parent.on_release(move || h.lock().unwrap_or_else(|p| p.into_inner()).clear());
    let child = parent.fork();
    drop(child);
    assert_eq!(
        held.lock().unwrap().len(),
        1,
        "a fork dying leaves the parent's container alone"
    );
    drop(parent);
    assert!(
        held.lock().unwrap().is_empty(),
        "the parent dying runs its hook"
    );
}
