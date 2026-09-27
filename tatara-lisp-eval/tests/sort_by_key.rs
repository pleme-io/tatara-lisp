//! `sort-by-key`: a stable, keyed, O(n log n) sort with `compare`'s rules.
//!
//! Before it, the only native sort was `sort-by`, an insertion sort taking a
//! comparator (n^2 calls), and an embedder's own sorts (blue's `junjo`) were
//! recursive and overflowed the stack near 600 elements. A design pipeline
//! sorting thousands of triangles by depth had no sort at all.
//!
//! Red run, 2026-09-27: with the NaN check removed from `sort_keyed_values`,
//! `a_nan_key_is_refused` failed with `NaN: [Float(1), Float(NaN), Float(2)]`
//! (the NaN sorted as if it were a number).

use tatara_lisp_eval::{install_full_stdlib_with, EvalError, Interpreter, Value};

fn eval(src: &str) -> Result<Value, EvalError> {
    let mut interp: Interpreter<()> = Interpreter::new();
    let mut host = ();
    install_full_stdlib_with(&mut interp, &mut host);
    let forms = tatara_lisp_eval::read_spanned(src).expect("parse");
    interp.eval_program(&forms, &mut host)
}

fn text(src: &str) -> String {
    format!("{}", eval(src).unwrap_or_else(|e| panic!("{src}: {e:?}")))
}

#[test]
fn orders_by_key_and_keeps_ties_in_input_order() {
    // Keyed by the first element; the two 1s keep their input order (b, d).
    assert_eq!(
        text("(sort-by-key (lambda (p) (car p)) (list (list 3 \"a\") (list 1 \"b\") (list 2 \"c\") (list 1 \"d\")))"),
        "((1 \"b\") (1 \"d\") (2 \"c\") (3 \"a\"))"
    );
}

#[test]
fn the_empty_list_and_one_element() {
    assert_eq!(text("(sort-by-key (lambda (x) x) (list))"), "()");
    assert_eq!(text("(sort-by-key (lambda (x) x) (list 5))"), "(5)");
}

#[test]
fn strings_mixed_numbers_and_negation_as_a_descending_key() {
    assert_eq!(
        text("(sort-by-key (lambda (s) s) (list \"pear\" \"apple\" \"fig\"))"),
        "(\"apple\" \"fig\" \"pear\")"
    );
    // An int and a float compare as numbers, as `compare` does.
    assert_eq!(
        text("(sort-by-key (lambda (x) x) (list 2 1.5 1))"),
        "(1 1.5 2)"
    );
    assert_eq!(
        text("(sort-by-key (lambda (x) (- 0 x)) (list 1 3 2))"),
        "(3 2 1)"
    );
}

#[test]
fn a_long_list_sorts_without_running_out_of_stack() {
    // 100,000 elements in reverse: far past the length at which every
    // recursive sort aborts the process. The list is built in Rust, because
    // building it in Lisp with `range` recurses once per element and aborts
    // first (measured: this test's first version died there, not in the sort).
    let mut interp: Interpreter<()> = Interpreter::new();
    let mut host = ();
    install_full_stdlib_with(&mut interp, &mut host);
    interp.define_global("big", Value::list((0..100_000).rev().map(Value::Int)));
    let forms = tatara_lisp_eval::read_spanned("(sort-by-key (lambda (x) x) big)").expect("parse");
    let v = interp.eval_program(&forms, &mut host).expect("sorted");
    let xs = match v {
        Value::List(xs) => xs,
        other => panic!("expected a list, got {other:?}"),
    };
    assert_eq!(xs.len(), 100_000);
    assert!(matches!(xs[0], Value::Int(0)));
    assert!(matches!(xs[99_999], Value::Int(99_999)));
}

#[test]
fn keys_of_different_kinds_are_refused() {
    assert!(eval("(sort-by-key (lambda (x) x) (list 1 \"a\"))").is_err());
}

#[test]
fn a_nan_key_is_refused() {
    let err = eval("(sort-by-key (lambda (x) x) (list 1.0 (sqrt -1.0) 2.0))").expect_err("NaN");
    assert!(
        err.short_message().contains("NaN"),
        "{}",
        err.short_message()
    );
}
