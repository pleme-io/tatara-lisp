//! `(string->number STR)` — the text→number border.
//!
//! Every value a script gets from outside itself is text: a captured stdout, a
//! regex group, a JSON string field. Without this function there is no typed
//! way across that border, and the two workarounds are both worse than the gap.
//! Folding over `string-chars` handles neither a sign nor a decimal point, and
//! pushing the arithmetic into a shell one-liner is exactly what the no-shell
//! rule exists to prevent.
//!
//! The two properties worth pinning, because both are silent when wrong:
//!
//!   * an integer comes back as **Int**, not Float. `"60"` has to compare and
//!     add against other Ints; a float slipping in here contaminates every
//!     downstream comparison and nothing reports it.
//!   * a non-number is a typed **error**, never 0. A 0 returned for `"abc"` is
//!     the same defect shape as an unreachable host reported as an empty one —
//!     a refusal wearing the costume of an answer.

use tatara_lisp_script::{eval_str, Value};

fn int_of(src: &str) -> i64 {
    match eval_str(src).unwrap_or_else(|e| panic!("{src} failed: {e}")) {
        Value::Int(n) => n,
        other => panic!("{src}: expected an Int, got {other:?}"),
    }
}

fn float_of(src: &str) -> f64 {
    match eval_str(src).unwrap_or_else(|e| panic!("{src} failed: {e}")) {
        Value::Float(f) => f,
        other => panic!("{src}: expected a Float, got {other:?}"),
    }
}

fn bool_of(src: &str) -> bool {
    match eval_str(src).unwrap_or_else(|e| panic!("{src} failed: {e}")) {
        Value::Bool(b) => b,
        other => panic!("{src}: expected a Bool, got {other:?}"),
    }
}

#[test]
fn integers_come_back_as_int_not_float() {
    // int_of panics on a Float, so this pins the VARIANT, not just the value.
    assert_eq!(int_of(r#"(string->number "60")"#), 60);
    assert_eq!(int_of(r#"(string->number "0")"#), 0);
    assert_eq!(int_of(r#"(string->number "-17")"#), -17);
    // 10-digit Unix epochs are the motivating case: an expiry read out of a
    // credential cache, compared against another epoch.
    assert_eq!(int_of(r#"(string->number "1789511970")"#), 1_789_511_970);
}

#[test]
fn surrounding_whitespace_is_tolerated() {
    // Captured stdout almost always carries a trailing newline.
    assert_eq!(int_of(r#"(string->number "  42\n")"#), 42);
}

#[test]
fn decimals_come_back_as_float() {
    assert!((float_of(r#"(string->number "54.193")"#) - 54.193).abs() < 1e-9);
}

#[test]
fn the_result_is_usable_as_a_number() {
    // The whole point: arithmetic on text that came from outside.
    assert_eq!(int_of(r#"(+ (string->number "1700") 100)"#), 1800);
    assert!(bool_of(r#"(> (string->number "1800") (string->number "1799"))"#));
}

#[test]
fn a_non_number_is_an_error_and_never_zero() {
    // The negative control. If this ever returns Int(0) the function has become
    // a silent-corruption engine: every typo parses, every comparison passes.
    for bad in [
        r#"(string->number "abc")"#,
        r#"(string->number "")"#,
        r#"(string->number "12x")"#,
    ] {
        assert!(
            eval_str(bad).is_err(),
            "{bad} should be an error, not a value"
        );
    }
}

#[test]
fn infinities_and_nan_are_refused() {
    // `"inf"` and `"NaN"` parse as f64 and would poison every comparison they
    // reach — an RTO of NaN compares false against every target, so a drill
    // would report neither pass nor fail.
    for bad in [
        r#"(string->number "inf")"#,
        r#"(string->number "-inf")"#,
        r#"(string->number "NaN")"#,
    ] {
        assert!(
            eval_str(bad).is_err(),
            "{bad} should be an error, not a value"
        );
    }
}
