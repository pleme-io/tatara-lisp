//! A VM-compiled function bound under two names dies with its interpreter.
//!
//! `(define g f)` binds a second global to the same compiled closure, so its
//! handle count is two; the release proof counted an aliased `Foreign` as zero
//! and kept the whole ring. blue's `spawn_server` defines its handler this
//! way (`(define %handler h)`), and a server left about 80 KB behind per run;
//! `g = f` alone left 11 KB on the VM and nothing on the walker (blue
//! docs/pure-blue/front-tower-conformance.md, C2, measured 2026-11-20).

use std::any::Any;
use std::sync::{Arc, Weak};

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, Value};

fn compiled(src: &str, keep: bool) -> (Weak<dyn Any + Send + Sync>, Option<Value>) {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i.eval_program_vm(&read_spanned(src).expect("reads"), &mut ())
        .expect("runs");
    let Some(Value::Foreign(f)) = i.lookup_global("f") else {
        panic!("f is a compiled closure");
    };
    let weak = Arc::downgrade(&f);
    let kept = keep.then(|| Value::Foreign(f.clone()));
    drop(f);
    drop(i);
    (weak, kept)
}

#[test]
fn an_aliased_compiled_function_dies_with_its_interpreter() {
    let (weak, _) = compiled("(define (f x) x) (define g f) (g 1)", false);
    assert!(
        weak.upgrade().is_none(),
        "the aliased compiled closure outlived its interpreter"
    );
}

#[test]
fn an_aliased_compiled_function_held_outside_stays_alive() {
    let (weak, kept) = compiled("(define (f x) x) (define g f) (g 1)", true);
    assert!(
        weak.upgrade().is_some(),
        "a closure held outside the frame was released"
    );
    drop(kept);
}
