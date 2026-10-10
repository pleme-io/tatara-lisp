//! A VM closure keeps working when a later chunk calls it.
//!
//! Every `eval_program_vm` compiles a fresh `Chunk`, and a closure the VM
//! makes reads its constants, global names and nested functions out of the
//! chunk it was compiled into. A REPL, a hot-code swap or any embedder that
//! defines in one program and calls from the next hands the VM a closure from
//! ANOTHER chunk.

use tatara_lisp::read_spanned;
use tatara_lisp_eval::{install_full_stdlib_with, Interpreter, Value};

fn interp() -> Interpreter<()> {
    let mut i = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i
}

fn vm(i: &mut Interpreter<()>, src: &str) -> Value {
    let forms = read_spanned(src).expect("read");
    i.eval_program_vm(&forms, &mut ()).expect("eval")
}

#[test]
fn a_closure_defined_by_one_program_runs_when_the_next_calls_it() {
    let mut i = interp();
    vm(&mut i, r#"(define (greet n) (list "hello" n (+ n 41)))"#);
    let out = vm(&mut i, "(greet 1)");
    assert_eq!(out.to_string(), r#"("hello" 1 42)"#);
}

/// The closure's own closures, tail calls and global writes read the same
/// chunk: a nested `lambda`, a self tail call and a `define` from inside.
#[test]
fn nested_closures_tail_calls_and_global_writes_stay_in_their_chunk() {
    let mut i = interp();
    vm(
        &mut i,
        r#"(define (adder k) (lambda (x) (+ x k 1000)))
           (define (count-down n) (if (= n 0) "done" (count-down (- n 1))))
           (define (remember v) (define remembered (list "kept" v)) remembered)"#,
    );
    assert_eq!(vm(&mut i, "((adder 5) 1)").to_string(), "1006");
    assert_eq!(vm(&mut i, "(count-down 50)").to_string(), r#""done""#);
    assert_eq!(vm(&mut i, "(remember 3)").to_string(), r#"("kept" 3)"#);
}
