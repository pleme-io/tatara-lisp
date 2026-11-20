//! A closure kept in a list that only its frames hold dies with its
//! interpreter.
//!
//! `defstrategy` and `defvisitor` keep their handlers as the `&rest` list of
//! a constructor whose returned lambda closes over it, and the handlers close
//! over the globals that bind the strategy. The release proof counted every
//! list as zero, because persistent lists share tree nodes, so the ring held:
//! about 16 KB a run in blue (docs/pure-blue/front-tower-conformance.md, C2).
//! An inline list keeps its elements in itself, sharing no node, so a list
//! held only by the frames the proof walks is counted through.

use std::sync::{Arc, Weak};

use tatara_lisp_eval::value::Closure;
use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, Value};

fn made(src: &str, name: &str) -> Weak<Closure> {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i.eval_program(&read_spanned(src).expect("reads"), &mut ())
        .expect("runs");
    let Some(Value::Closure(f)) = i.lookup_global(name) else {
        panic!("{name} is a closure");
    };
    let weak = Arc::downgrade(&f);
    drop(f);
    drop(i);
    weak
}

#[test]
fn a_strategy_and_a_visitor_die_with_their_interpreter() {
    for (src, name) in [
        ("(defstrategy st :x (lambda () 1) :default (lambda () 2))", "st"),
        ("(defvisitor vis :x (lambda (n) n) :default (lambda (n) 0))", "vis"),
        ("(define (mk &rest kvs) (let ((h kvs)) (lambda (k) (plist-get h k)))) (define s (mk :a (lambda () 1)))", "s"),
    ] {
        assert!(made(src, name).upgrade().is_none(), "{name} outlived its interpreter");
    }
}

#[test]
fn a_strategy_still_answers_while_its_interpreter_lives() {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    let got = i
        .eval_program(
            &read_spanned("(defstrategy st :x (lambda () 1) :default (lambda () 2)) (list (strategy-call st :x) (strategy-call st :y))")
                .expect("reads"),
            &mut (),
        )
        .expect("runs");
    assert_eq!(format!("{got}"), "(1 2)");
}
