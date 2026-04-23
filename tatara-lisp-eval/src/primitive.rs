//! Built-in primitive procedures.
//!
//! `install_primitives` registers the standard set of procedures on an
//! `Interpreter<H>`. The functions are generic over `H` so they compose
//! with any embedder. Primitives never read or mutate host state — they
//! operate only on their `Value` arguments.
//!
//! The public contract is the set of names registered. Removing or
//! renaming any entry is a breaking change; adding entries is fine.

use std::sync::Arc;

use tatara_lisp::{Sexp, Span};

use crate::error::{EvalError, Result};
use crate::eval::Interpreter;
use crate::ffi::Arity;
use crate::value::Value;

/// Names of primitive procedures registered by `install_primitives`.
/// Kept in sync by a self-test — see `tests::names_match_installed`.
pub const PRIMITIVE_NAMES: &[&str] = &[
    // arithmetic
    "+",
    "-",
    "*",
    "/",
    "modulo",
    "abs",
    "min",
    "max",
    // comparison
    "=",
    "<",
    ">",
    "<=",
    ">=",
    // type predicates
    "null?",
    "pair?",
    "list?",
    "symbol?",
    "string?",
    "integer?",
    "number?",
    "boolean?",
    "procedure?",
    "foreign?",
    // lists
    "car",
    "cdr",
    "cons",
    "list",
    "length",
    "reverse",
    "append",
    // equality
    "eq?",
    "equal?",
    // strings
    "string-length",
    "string-append",
    // IO (embedder may replace)
    "display",
    "newline",
    "print",
];

/// Register the standard primitive set on `interp`.
pub fn install_primitives<H: 'static>(interp: &mut Interpreter<H>) {
    // ── Arithmetic ────────────────────────────────────────────────
    interp.register_fn("+", Arity::AtLeast(0), |args: &[Value], _h: &mut H, sp| {
        reduce_numeric(args, sp, 0, 0.0, |a, b| a + b, |a, b| a + b)
    });
    interp.register_fn("-", Arity::AtLeast(1), prim_sub::<H>);
    interp.register_fn("*", Arity::AtLeast(0), |args: &[Value], _h: &mut H, sp| {
        reduce_numeric(args, sp, 1, 1.0, |a, b| a * b, |a, b| a * b)
    });
    interp.register_fn("/", Arity::AtLeast(1), prim_div::<H>);
    interp.register_fn(
        "modulo",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let a = expect_int(&args[0], sp)?;
            let b = expect_int(&args[1], sp)?;
            if b == 0 {
                return Err(EvalError::DivisionByZero { at: sp });
            }
            Ok(Value::Int(a.rem_euclid(b)))
        },
    );
    interp.register_fn(
        "abs",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Int(n) => Ok(Value::Int(n.abs())),
            Value::Float(n) => Ok(Value::Float(n.abs())),
            other => Err(EvalError::type_mismatch("number", other.type_name(), sp)),
        },
    );
    interp.register_fn(
        "min",
        Arity::AtLeast(1),
        |args: &[Value], _h: &mut H, sp| {
            reduce_numeric(args, sp, i64::MAX, f64::INFINITY, i64::min, f64::min)
        },
    );
    interp.register_fn(
        "max",
        Arity::AtLeast(1),
        |args: &[Value], _h: &mut H, sp| {
            reduce_numeric(args, sp, i64::MIN, f64::NEG_INFINITY, i64::max, f64::max)
        },
    );

    // ── Comparison ────────────────────────────────────────────────
    interp.register_fn("=", Arity::AtLeast(2), |args: &[Value], _h: &mut H, sp| {
        cmp_chain(args, sp, |a, b| a == b, |a, b| a == b)
    });
    interp.register_fn("<", Arity::AtLeast(2), |args: &[Value], _h: &mut H, sp| {
        cmp_chain(args, sp, |a, b| a < b, |a, b| a < b)
    });
    interp.register_fn(">", Arity::AtLeast(2), |args: &[Value], _h: &mut H, sp| {
        cmp_chain(args, sp, |a, b| a > b, |a, b| a > b)
    });
    interp.register_fn("<=", Arity::AtLeast(2), |args: &[Value], _h: &mut H, sp| {
        cmp_chain(args, sp, |a, b| a <= b, |a, b| a <= b)
    });
    interp.register_fn(">=", Arity::AtLeast(2), |args: &[Value], _h: &mut H, sp| {
        cmp_chain(args, sp, |a, b| a >= b, |a, b| a >= b)
    });

    // ── Predicates ────────────────────────────────────────────────
    interp.register_fn("null?", Arity::Exact(1), |a: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(matches!(&a[0], Value::Nil)))
    });
    interp.register_fn("pair?", Arity::Exact(1), |a: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(
            matches!(&a[0], Value::List(xs) if !xs.is_empty()),
        ))
    });
    interp.register_fn("list?", Arity::Exact(1), |a: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(matches!(&a[0], Value::List(_) | Value::Nil)))
    });
    interp.register_fn(
        "symbol?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(Value::Bool(matches!(&a[0], Value::Symbol(_)))),
    );
    interp.register_fn(
        "string?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(Value::Bool(matches!(&a[0], Value::Str(_)))),
    );
    interp.register_fn(
        "integer?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(Value::Bool(matches!(&a[0], Value::Int(_)))),
    );
    interp.register_fn(
        "number?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| {
            Ok(Value::Bool(matches!(
                &a[0],
                Value::Int(_) | Value::Float(_)
            )))
        },
    );
    interp.register_fn(
        "boolean?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(Value::Bool(matches!(&a[0], Value::Bool(_)))),
    );
    interp.register_fn(
        "procedure?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| {
            Ok(Value::Bool(matches!(
                &a[0],
                Value::Closure(_) | Value::NativeFn(_)
            )))
        },
    );
    interp.register_fn(
        "foreign?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(Value::Bool(matches!(&a[0], Value::Foreign(_)))),
    );

    // ── Lists ─────────────────────────────────────────────────────
    interp.register_fn("car", Arity::Exact(1), prim_car::<H>);
    interp.register_fn("cdr", Arity::Exact(1), prim_cdr::<H>);
    interp.register_fn("cons", Arity::Exact(2), prim_cons::<H>);
    interp.register_fn("list", Arity::Any, |args: &[Value], _h: &mut H, _sp| {
        Ok(Value::list(args.iter().cloned()))
    });
    interp.register_fn(
        "length",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Nil => Ok(Value::Int(0)),
            Value::List(xs) => Ok(Value::Int(xs.len() as i64)),
            other => Err(EvalError::type_mismatch("list", other.type_name(), sp)),
        },
    );
    interp.register_fn(
        "reverse",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Nil => Ok(Value::Nil),
            Value::List(xs) => {
                let mut v = xs.as_ref().clone();
                v.reverse();
                Ok(Value::List(Arc::new(v)))
            }
            other => Err(EvalError::type_mismatch("list", other.type_name(), sp)),
        },
    );
    interp.register_fn("append", Arity::Any, prim_append::<H>);

    // ── Equality ──────────────────────────────────────────────────
    interp.register_fn("eq?", Arity::Exact(2), |a: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(value_eq_shallow(&a[0], &a[1])))
    });
    interp.register_fn("equal?", Arity::Exact(2), |a: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(value_eq_deep(&a[0], &a[1])))
    });

    // ── Strings ───────────────────────────────────────────────────
    interp.register_fn(
        "string-length",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
            other => Err(EvalError::type_mismatch("string", other.type_name(), sp)),
        },
    );
    interp.register_fn(
        "string-append",
        Arity::Any,
        |args: &[Value], _h: &mut H, sp| {
            let mut out = String::new();
            for a in args {
                match a {
                    Value::Str(s) => out.push_str(s.as_ref()),
                    other => return Err(EvalError::type_mismatch("string", other.type_name(), sp)),
                }
            }
            Ok(Value::Str(Arc::from(out)))
        },
    );

    // ── IO (embedder may substitute) ─────────────────────────────
    interp.register_fn(
        "display",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, _sp| {
            print!("{}", args[0]);
            Ok(Value::Nil)
        },
    );
    interp.register_fn(
        "newline",
        Arity::Exact(0),
        |_args: &[Value], _h: &mut H, _sp| {
            println!();
            Ok(Value::Nil)
        },
    );
    interp.register_fn(
        "print",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, _sp| {
            println!("{}", args[0]);
            Ok(Value::Nil)
        },
    );
}

// ── Helpers ───────────────────────────────────────────────────────

fn expect_int(v: &Value, sp: Span) -> Result<i64> {
    match v {
        Value::Int(n) => Ok(*n),
        other => Err(EvalError::type_mismatch("integer", other.type_name(), sp)),
    }
}

fn as_number_either(v: &Value, sp: Span) -> Result<NumVal> {
    match v {
        Value::Int(n) => Ok(NumVal::I(*n)),
        Value::Float(n) => Ok(NumVal::F(*n)),
        other => Err(EvalError::type_mismatch("number", other.type_name(), sp)),
    }
}

#[derive(Clone, Copy)]
enum NumVal {
    I(i64),
    F(f64),
}

impl NumVal {
    fn to_float(self) -> f64 {
        match self {
            Self::I(n) => n as f64,
            Self::F(n) => n,
        }
    }
}

fn reduce_numeric(
    args: &[Value],
    sp: Span,
    int_init: i64,
    float_init: f64,
    fi: impl Fn(i64, i64) -> i64,
    ff: impl Fn(f64, f64) -> f64,
) -> Result<Value> {
    // All-int fast path. Promote to float on first Float encountered.
    let mut saw_float = false;
    let mut acc_i = int_init;
    let mut acc_f = float_init;
    for a in args {
        match as_number_either(a, sp)? {
            NumVal::I(n) => {
                if saw_float {
                    acc_f = ff(acc_f, n as f64);
                } else {
                    acc_i = fi(acc_i, n);
                }
            }
            NumVal::F(n) => {
                if !saw_float {
                    acc_f = ff(acc_i as f64, n);
                    saw_float = true;
                } else {
                    acc_f = ff(acc_f, n);
                }
            }
        }
    }
    if saw_float {
        Ok(Value::Float(acc_f))
    } else {
        Ok(Value::Int(acc_i))
    }
}

fn prim_sub<H: 'static>(args: &[Value], _h: &mut H, sp: Span) -> Result<Value> {
    if args.len() == 1 {
        return match as_number_either(&args[0], sp)? {
            NumVal::I(n) => Ok(Value::Int(-n)),
            NumVal::F(n) => Ok(Value::Float(-n)),
        };
    }
    let first = as_number_either(&args[0], sp)?;
    let mut saw_float = matches!(first, NumVal::F(_));
    let mut acc_i: i64 = if let NumVal::I(n) = first { n } else { 0 };
    let mut acc_f: f64 = first.to_float();
    for a in &args[1..] {
        match as_number_either(a, sp)? {
            NumVal::I(n) => {
                if saw_float {
                    acc_f -= n as f64;
                } else {
                    acc_i -= n;
                }
            }
            NumVal::F(n) => {
                if !saw_float {
                    acc_f = (acc_i as f64) - n;
                    saw_float = true;
                } else {
                    acc_f -= n;
                }
            }
        }
    }
    Ok(if saw_float {
        Value::Float(acc_f)
    } else {
        Value::Int(acc_i)
    })
}

fn prim_div<H: 'static>(args: &[Value], _h: &mut H, sp: Span) -> Result<Value> {
    if args.len() == 1 {
        return match as_number_either(&args[0], sp)? {
            NumVal::I(n) => {
                if n == 0 {
                    Err(EvalError::DivisionByZero { at: sp })
                } else {
                    Ok(Value::Float(1.0 / (n as f64)))
                }
            }
            NumVal::F(n) => {
                if n == 0.0 {
                    Err(EvalError::DivisionByZero { at: sp })
                } else {
                    Ok(Value::Float(1.0 / n))
                }
            }
        };
    }
    let first = as_number_either(&args[0], sp)?;
    let mut saw_float = matches!(first, NumVal::F(_));
    let mut acc_i: i64 = if let NumVal::I(n) = first { n } else { 0 };
    let mut acc_f: f64 = first.to_float();
    for a in &args[1..] {
        let b = as_number_either(a, sp)?;
        let zero = matches!(b, NumVal::I(0)) || matches!(b, NumVal::F(n) if n == 0.0);
        if zero {
            return Err(EvalError::DivisionByZero { at: sp });
        }
        match b {
            NumVal::I(n) => {
                if saw_float {
                    acc_f /= n as f64;
                } else if acc_i % n == 0 {
                    acc_i /= n;
                } else {
                    acc_f = acc_i as f64 / n as f64;
                    saw_float = true;
                }
            }
            NumVal::F(n) => {
                if !saw_float {
                    acc_f = acc_i as f64 / n;
                    saw_float = true;
                } else {
                    acc_f /= n;
                }
            }
        }
    }
    Ok(if saw_float {
        Value::Float(acc_f)
    } else {
        Value::Int(acc_i)
    })
}

fn cmp_chain(
    args: &[Value],
    sp: Span,
    cmp_i: impl Fn(i64, i64) -> bool,
    cmp_f: impl Fn(f64, f64) -> bool,
) -> Result<Value> {
    for w in args.windows(2) {
        let ok = match (as_number_either(&w[0], sp)?, as_number_either(&w[1], sp)?) {
            (NumVal::I(a), NumVal::I(b)) => cmp_i(a, b),
            (a, b) => cmp_f(a.to_float(), b.to_float()),
        };
        if !ok {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn prim_car<H: 'static>(args: &[Value], _h: &mut H, sp: Span) -> Result<Value> {
    match &args[0] {
        Value::List(xs) if !xs.is_empty() => Ok(xs[0].clone()),
        Value::Nil | Value::List(_) => Err(EvalError::native_fn(
            Arc::<str>::from("car"),
            "car of empty list",
            sp,
        )),
        other => Err(EvalError::type_mismatch("pair", other.type_name(), sp)),
    }
}

fn prim_cdr<H: 'static>(args: &[Value], _h: &mut H, sp: Span) -> Result<Value> {
    match &args[0] {
        Value::List(xs) if !xs.is_empty() => {
            if xs.len() == 1 {
                Ok(Value::Nil)
            } else {
                Ok(Value::List(Arc::new(xs[1..].to_vec())))
            }
        }
        Value::Nil | Value::List(_) => Err(EvalError::native_fn(
            Arc::<str>::from("cdr"),
            "cdr of empty list",
            sp,
        )),
        other => Err(EvalError::type_mismatch("pair", other.type_name(), sp)),
    }
}

fn prim_cons<H: 'static>(args: &[Value], _h: &mut H, _sp: Span) -> Result<Value> {
    let head = args[0].clone();
    let tail = &args[1];
    let mut v = Vec::new();
    v.push(head);
    match tail {
        Value::Nil => {}
        Value::List(xs) => v.extend(xs.iter().cloned()),
        other => v.push(other.clone()),
    }
    Ok(Value::List(Arc::new(v)))
}

fn prim_append<H: 'static>(args: &[Value], _h: &mut H, sp: Span) -> Result<Value> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Value::Nil => {}
            Value::List(xs) => out.extend(xs.iter().cloned()),
            other => return Err(EvalError::type_mismatch("list", other.type_name(), sp)),
        }
    }
    if out.is_empty() {
        Ok(Value::Nil)
    } else {
        Ok(Value::List(Arc::new(out)))
    }
}

fn value_eq_shallow(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => Arc::ptr_eq(a, b) || a == b,
        (Value::Symbol(a), Value::Symbol(b)) => a == b,
        (Value::Keyword(a), Value::Keyword(b)) => a == b,
        (Value::List(a), Value::List(b)) => Arc::ptr_eq(a, b),
        (Value::NativeFn(a), Value::NativeFn(b)) => a.name == b.name,
        (Value::Closure(a), Value::Closure(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

fn value_eq_deep(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::List(a), Value::List(b)) => {
            if a.len() != b.len() {
                return false;
            }
            a.iter().zip(b.iter()).all(|(x, y)| value_eq_deep(x, y))
        }
        (Value::Sexp(a, _), Value::Sexp(b, _)) => sexp_eq(a, b),
        _ => value_eq_shallow(a, b),
    }
}

fn sexp_eq(a: &Sexp, b: &Sexp) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for n in PRIMITIVE_NAMES {
            assert!(seen.insert(*n), "duplicate primitive: {n}");
        }
    }

    #[test]
    fn names_match_installed() {
        // Every name in PRIMITIVE_NAMES must resolve to a NativeFn after
        // install_primitives.
        struct NoHost;
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        for name in PRIMITIVE_NAMES {
            let v = i
                .lookup_global(name)
                .unwrap_or_else(|| panic!("primitive `{name}` not installed"));
            assert!(
                matches!(v, Value::NativeFn(_)),
                "`{name}` is not a native-fn: {v:?}"
            );
        }
    }
}
