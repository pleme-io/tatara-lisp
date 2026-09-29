//! Integer arithmetic is exact or refused; wrapping is asked for by name.
//!
//! Measured before (tatara-script 0.3.62, a release build): `(* 9223372036854775807 2)`
//! answered `-2`, `(- 0 9223372036854775807 2)` answered `9223372036854775807`
//! and `(abs i64::MIN)` answered `i64::MIN`. The same operations PANICKED in a
//! debug build, so the answer was a function of the build profile. Every test
//! here runs in both profiles (`cargo test` and `cargo test --release`) and
//! means the same thing in each.
//!
//! Red run, recorded 2026-09-29: with `+` restored to `|a, b| Some(a.wrapping_add(b))`
//! in `install_primitives`, `every_operator_agrees_with_exact_arithmetic` fails
//! on the first overflowing pair of the seeded corpus (`expected overflow, got
//! Int(..)`), and `overflow_is_an_error_not_a_wrap` fails on `+`.

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, EvalError, Interpreter, Value};

fn eval(src: &str) -> Result<Value, EvalError> {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    let forms = read_spanned(src).expect("reads");
    i.eval_program(&forms, &mut ())
}

fn overflows(src: &str, op: &str) {
    match eval(src) {
        Err(EvalError::IntegerOverflow { op: got, .. }) => assert_eq!(got, op, "{src}"),
        other => panic!("{src}: expected an overflow in `{op}`, got {other:?}"),
    }
}

const MAX: i64 = i64::MAX;
const MIN: i64 = i64::MIN;

#[test]
fn overflow_is_an_error_not_a_wrap() {
    overflows(&format!("(* {MAX} 2)"), "*");
    overflows(&format!("(+ {MAX} 1)"), "+");
    overflows(&format!("(- 0 {MAX} 2)"), "-");
    overflows(&format!("(abs (- 0 {MAX} 1))"), "abs");
    overflows(&format!("(- (- 0 {MAX} 1))"), "-");
    overflows(&format!("(/ (- 0 {MAX} 1) -1)"), "/");
    overflows(&format!("(modulo (- 0 {MAX} 1) -1)"), "modulo");
    overflows("(expt 2 63)", "expt");
    overflows("(expt 2 64)", "expt");
    overflows(&format!("(gcd (- 0 {MAX} 1) 2)"), "gcd");
    overflows(&format!("(lcm {MAX} 2)"), "lcm");
    overflows("(floor 1e300)", "floor");
    overflows("(round (sqrt -1.0))", "round");
    overflows("(truncate -1e19)", "truncate");
}

#[test]
fn the_error_is_catchable_and_tagged() {
    let v = eval(&format!("(try (* {MAX} 2) (catch (e) (error-tag e)))")).unwrap();
    assert_eq!(format!("{v}"), ":integer-overflow");
    let msg = eval(&format!("(* {MAX} 2)")).unwrap_err().short_message();
    assert_eq!(msg, "integer overflow in `*`");
}

/// The edges that DO fit still compute exactly.
#[test]
fn results_at_the_edge_are_exact() {
    let cases = [
        (format!("(+ {MAX} 0)"), MAX),
        (format!("(- 0 {MAX} 1)"), MIN),
        (format!("(* {MIN} 1)"), MIN),
        (format!("(abs (- 0 {MAX}))"), MAX),
        (format!("(/ {MIN} 1)"), MIN),
        (format!("(modulo {MIN} 1)"), 0),
        ("(expt 2 62)".to_string(), 1 << 62),
        ("(expt -2 63)".to_string(), MIN),
        ("(expt 1 1000000)".to_string(), 1),
        ("(floor -9223372036854775808.0)".to_string(), MIN),
    ];
    for (src, want) in cases {
        match eval(&src) {
            Ok(Value::Int(n)) => assert_eq!(n, want, "{src}"),
            other => panic!("{src}: expected {want}, got {other:?}"),
        }
    }
}

/// Wrapping is explicit, and wraps.
#[test]
fn wrapping_is_asked_for_by_name() {
    let cases = [
        (format!("(wrapping-mul {MAX} 2)"), -2),
        (format!("(wrapping-add {MAX} 1)"), MIN),
        (format!("(wrapping-sub 0 {MAX} 2)"), MAX),
        (format!("(wrapping-sub {MIN})"), MIN),
        ("(wrapping-add)".to_string(), 0),
        ("(wrapping-mul)".to_string(), 1),
        ("(wrapping-add 2 3)".to_string(), 5),
    ];
    for (src, want) in cases {
        match eval(&src) {
            Ok(Value::Int(n)) => assert_eq!(n, want, "{src}"),
            other => panic!("{src}: expected {want}, got {other:?}"),
        }
    }
    assert!(matches!(
        eval("(wrapping-add 1 2.5)"),
        Err(EvalError::TypeMismatch { .. })
    ));
}

/// Differential against exact (`i128`) arithmetic on a seeded corpus biased to
/// the edges: every operator either returns the exact result or refuses with
/// an overflow, never anything else, and the wrapping forms return the exact
/// result reduced mod 2^64.
#[test]
fn every_operator_agrees_with_exact_arithmetic() {
    let mut state: u64 = 0x5eed_0000_0000_0005;
    let mut next = move || {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let edges = [
        0,
        1,
        -1,
        2,
        -2,
        MAX,
        MIN,
        MAX - 1,
        MIN + 1,
        1 << 32,
        -(1 << 32),
    ];
    let mut pick = move || -> i64 {
        let r = next();
        if r % 3 == 0 {
            edges[(r / 3 % edges.len() as u64) as usize]
        } else {
            r as i64
        }
    };
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    let mut run = |src: String| {
        let forms = read_spanned(&src).unwrap();
        (src.clone(), i.eval_program(&forms, &mut ()))
    };
    let mut overflowed = 0;
    for _ in 0..3_000 {
        let (a, b) = (pick(), pick());
        for (op, wrap, exact) in [
            ("+", "wrapping-add", i128::from(a) + i128::from(b)),
            ("-", "wrapping-sub", i128::from(a) - i128::from(b)),
            ("*", "wrapping-mul", i128::from(a) * i128::from(b)),
        ] {
            let (src, got) = run(format!("({op} {a} {b})"));
            match (i64::try_from(exact), got) {
                (Ok(want), Ok(Value::Int(n))) => assert_eq!(n, want, "{src}"),
                (Err(_), Err(EvalError::IntegerOverflow { .. })) => overflowed += 1,
                (want, got) => panic!("{src}: exact {exact} ({want:?}), got {got:?}"),
            }
            let (src, got) = run(format!("({wrap} {a} {b})"));
            #[allow(clippy::cast_possible_truncation)]
            let want = exact as i64;
            assert!(
                matches!(got, Ok(Value::Int(n)) if n == want),
                "{src}: {got:?}"
            );
        }
    }
    assert!(
        overflowed > 100,
        "the corpus must reach the edges: {overflowed}"
    );
}
