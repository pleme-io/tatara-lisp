use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, EvalError, Interpreter, Value};

fn walk(src: &str) -> Result<Value, EvalError> {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    let forms = read_spanned(src).expect("reads");
    i.eval_program(&forms, &mut ())
}

fn int(src: &str) -> i64 {
    match walk(src) {
        Ok(Value::Int(n)) => n,
        other => panic!("{src}: {other:?}"),
    }
}

#[test]
fn a_parameter_shadows_a_global_and_a_let_shadows_the_parameter() {
    assert_eq!(int("(define x 1) (define (f x) x) (f 2)"), 2);
    assert_eq!(int("(define (f x) (let ((x 3)) x)) (f 2)"), 3);
    assert_eq!(int("(define (f x) (+ (let ((x 3)) x) x)) (f 2)"), 5);
    assert_eq!(
        int("(define (f x) ((lambda (x) (* x 10)) (+ x 1))) (f 2)"),
        30
    );
    assert_eq!(int("(define x 1) (define (f y) x) (f 2)"), 1);
}

#[test]
fn a_define_in_a_body_shadows_its_own_parameter() {
    assert_eq!(int("(define (f x) (define x (* x 7)) x) (f 3)"), 21);
    assert_eq!(
        int("(define (f x) (define y 4) (define x 5) (+ x y)) (f 3)"),
        9
    );
}

#[test]
fn a_closure_sees_a_define_made_after_it_captured_the_frame() {
    assert_eq!(
        int("(define (f a) (define get (lambda () later)) (define later (* a 2)) (get)) (f 21)"),
        42
    );
}

#[test]
fn recursion_resolves_through_a_function_defined_later() {
    assert_eq!(
        int("(define (ev n) (if (= n 0) 1 (od (- n 1)))) (define (od n) (if (= n 0) 0 (ev (- n 1)))) (ev 10)"),
        1
    );
}

#[test]
fn set_through_a_closure_writes_the_captured_parameter() {
    assert_eq!(
        int("(define (counter n) (lambda () (set! n (+ n 1)) n)) (define c (counter 5)) (c) (c) (c)"),
        8
    );
    assert_eq!(
        int("(define (f a b) (define g (lambda () (set! b (* b 10)) (set! a (+ a 1)))) (g) (g) (+ a b)) (f 1 2)"),
        203
    );
}

#[test]
fn a_frame_keeps_every_binding_past_the_point_it_changes_shape() {
    let program = "(define (f a b c d e f g h i j)
                     (define k 11)
                     (define l 12)
                     (set! j (* j 100))
                     (set! a (* a 100))
                     (+ a b c d e f g h i j k l))
                   (f 1 2 3 4 5 6 7 8 9 10)";
    assert_eq!(
        int(program),
        100 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 1000 + 11 + 12
    );
    assert_eq!(
        int("(let ((a 1)) (define b 2) (set! a 10) (define c 3) (set! b 20) (+ a b c))"),
        33
    );
}

#[test]
fn names_alike_in_length_and_outer_letters_stay_distinct() {
    assert_eq!(int("(let ((abcde 1) (azcwe 2)) (- abcde azcwe))"), -1);
    assert_eq!(
        int("(define (f abcde azcwe) (set! azcwe 7) (- abcde azcwe)) (f 1 2)"),
        -6
    );
    assert!(matches!(
        walk("(define (f abcde) azcwe) (f 1)"),
        Err(EvalError::UnboundSymbol { .. })
    ));
}

#[test]
fn a_repeated_parameter_binds_its_last_argument() {
    assert_eq!(int("((lambda (x x) x) 1 2)"), 2);
}

#[test]
fn a_rest_parameter_binds_the_remaining_arguments() {
    assert_eq!(int("((lambda (a &rest r) (+ a (length r))) 10 1 2 3)"), 13);
    assert_eq!(int("((lambda (&rest r) (length r)))"), 0);
}

#[test]
fn a_native_call_still_refuses_a_wrong_argument_count_after_its_arguments() {
    assert!(matches!(
        walk("(modulo 1 2 3)"),
        Err(EvalError::ArityMismatch { got: 3, .. })
    ));
    assert!(matches!(
        walk("(modulo 1 undefined-name 3)"),
        Err(EvalError::UnboundSymbol { .. })
    ));
}
