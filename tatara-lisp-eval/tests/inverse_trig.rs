//! The inverse trigonometric primitives: `asin`, `acos`, `atan`, `atan2`,
//! `hypot`.
//!
//! Before these, a consumer that needed an angle from a ratio had to search
//! for it: blue's `gyouretsu.arccos` ran a bisection over `cos` "because this
//! runtime has no `acos`". Values below are checked against closed forms
//! (pi/6, pi/4, pi/3, the 3-4-5 triangle), not against Rust's own output.
//!
//! Red run, 2026-09-27: with `acos` answering `n.acos()` unguarded,
//! `out_of_domain_is_refused_not_nan` failed with `(acos -2): Float(NaN)`.

use tatara_lisp_eval::{install_full_stdlib_with, EvalError, Interpreter, Value};

fn eval(src: &str) -> Result<Value, EvalError> {
    let mut interp: Interpreter<()> = Interpreter::new();
    let mut host = ();
    install_full_stdlib_with(&mut interp, &mut host);
    let forms = tatara_lisp_eval::read_spanned(src).expect("parse");
    interp.eval_program(&forms, &mut host)
}

fn float(src: &str) -> f64 {
    match eval(src) {
        Ok(Value::Float(x)) => x,
        other => panic!("{src}: expected a float, got {other:?}"),
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

const PI: f64 = std::f64::consts::PI;

#[test]
fn closed_form_values() {
    assert!(near(float("(asin 0.5)"), PI / 6.0));
    assert!(near(float("(acos 0.5)"), PI / 3.0));
    assert!(near(float("(atan 1)"), PI / 4.0));
    assert!(near(float("(hypot 3 4)"), 5.0));
}

#[test]
fn atan2_takes_y_first_and_covers_every_quadrant() {
    assert!(near(float("(atan2 1 1)"), PI / 4.0));
    assert!(near(float("(atan2 1 -1)"), 3.0 * PI / 4.0));
    assert!(near(float("(atan2 -1 -1)"), -3.0 * PI / 4.0));
    assert!(near(float("(atan2 -1 1)"), -PI / 4.0));
    // y first: (atan2 0 -1) is pi, (atan2 -1 0) is -pi/2.
    assert!(near(float("(atan2 0 -1)"), PI));
    assert!(near(float("(atan2 -1 0)"), -PI / 2.0));
}

#[test]
fn each_inverse_undoes_its_function() {
    for x in ["0.1", "0.5", "0.9", "-0.3"] {
        assert!(near(
            float(&format!("(sin (asin {x}))")),
            x.parse().unwrap()
        ));
        assert!(near(
            float(&format!("(cos (acos {x}))")),
            x.parse().unwrap()
        ));
        assert!(near(
            float(&format!("(tan (atan {x}))")),
            x.parse().unwrap()
        ));
    }
}

#[test]
fn the_domain_edges_are_accepted() {
    assert!(near(float("(asin 1)"), PI / 2.0));
    assert!(near(float("(acos -1)"), PI));
    assert!(near(float("(acos 1)"), 0.0));
}

#[test]
fn out_of_domain_is_refused_not_nan() {
    for src in ["(asin 1.0000001)", "(acos -2)", "(acos 2)"] {
        let err = eval(src).expect_err(src);
        assert!(
            err.short_message().contains("outside [-1, 1]"),
            "{src}: {}",
            err.short_message()
        );
    }
}

#[test]
fn hypot_does_not_overflow_where_the_squares_would() {
    let x = float("(hypot 1e200 1e200)");
    assert!(x.is_finite());
    assert!((x / 1e200 - std::f64::consts::SQRT_2).abs() < 1e-12);
}
