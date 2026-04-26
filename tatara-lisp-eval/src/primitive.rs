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
    "expt",
    "sqrt",
    "floor",
    "ceiling",
    "round",
    "truncate",
    "gcd",
    "lcm",
    "sin",
    "cos",
    "tan",
    "log",
    "exp",
    // comparison
    "=",
    "<",
    ">",
    "<=",
    ">=",
    "not=",
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
    "atom?",
    "keyword?",
    // lists
    "car",
    "cdr",
    "cons",
    "list",
    "length",
    "reverse",
    "append",
    "take",
    "drop",
    "nth",
    // equality
    "eq?",
    "equal?",
    // strings
    "string-length",
    "string-append",
    // string <-> symbol/keyword
    "symbol->string",
    "keyword->string",
    "string->symbol",
    "string->keyword",
    // IO (embedder may replace)
    "display",
    "newline",
    "print",
    // Hygiene helpers
    "gensym",
    // Structured errors
    "error",
    "ex-info",
    "throw",
    "error?",
    "error-tag",
    "error-message",
    "error-data",
    "error-data-get",
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
        // Scheme R7RS: null? holds for both `()` and the empty list literal.
        // We represent both as `Value::Nil` and `Value::List([])` — accept both.
        Ok(Value::Bool(match &a[0] {
            Value::Nil => true,
            Value::List(xs) => xs.is_empty(),
            _ => false,
        }))
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

    // ── More numeric ──────────────────────────────────────────────
    interp.register_fn(
        "expt",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| match (&args[0], &args[1]) {
            (Value::Int(b), Value::Int(e)) if *e >= 0 && *e < 64 => {
                let mut acc: i64 = 1;
                for _ in 0..*e {
                    acc = acc.wrapping_mul(*b);
                }
                Ok(Value::Int(acc))
            }
            (a, b) => {
                let af = as_number_either(a, sp)?.to_float();
                let bf = as_number_either(b, sp)?.to_float();
                Ok(Value::Float(af.powf(bf)))
            }
        },
    );
    interp.register_fn(
        "sqrt",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Float(n.sqrt()))
        },
    );
    interp.register_fn(
        "floor",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Int(n.floor() as i64))
        },
    );
    interp.register_fn(
        "ceiling",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Int(n.ceil() as i64))
        },
    );
    interp.register_fn(
        "round",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Int(n.round() as i64))
        },
    );
    interp.register_fn(
        "truncate",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Int(n.trunc() as i64))
        },
    );
    interp.register_fn(
        "gcd",
        Arity::AtLeast(0),
        |args: &[Value], _h: &mut H, sp| {
            if args.is_empty() {
                return Ok(Value::Int(0));
            }
            let mut g = expect_int(&args[0], sp)?.unsigned_abs() as i64;
            for a in &args[1..] {
                let b = expect_int(a, sp)?.unsigned_abs() as i64;
                g = gcd(g, b);
            }
            Ok(Value::Int(g))
        },
    );
    interp.register_fn(
        "lcm",
        Arity::AtLeast(0),
        |args: &[Value], _h: &mut H, sp| {
            if args.is_empty() {
                return Ok(Value::Int(1));
            }
            let mut l = expect_int(&args[0], sp)?.unsigned_abs() as i64;
            for a in &args[1..] {
                let b = expect_int(a, sp)?.unsigned_abs() as i64;
                if l == 0 || b == 0 {
                    l = 0;
                } else {
                    l = l / gcd(l, b) * b;
                }
            }
            Ok(Value::Int(l))
        },
    );
    interp.register_fn(
        "sin",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Float(n.sin()))
        },
    );
    interp.register_fn(
        "cos",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Float(n.cos()))
        },
    );
    interp.register_fn(
        "tan",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Float(n.tan()))
        },
    );
    interp.register_fn(
        "log",
        Arity::Range(1, 2),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            if args.len() == 1 {
                Ok(Value::Float(n.ln()))
            } else {
                let base = as_number_either(&args[1], sp)?.to_float();
                Ok(Value::Float(n.log(base)))
            }
        },
    );
    interp.register_fn(
        "exp",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let n = as_number_either(&args[0], sp)?.to_float();
            Ok(Value::Float(n.exp()))
        },
    );

    // ── More list ops ─────────────────────────────────────────────
    interp.register_fn(
        "take",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let n = expect_int(&args[0], sp)?.max(0) as usize;
            let xs = list_view(&args[1], sp)?;
            let take_n = n.min(xs.len());
            Ok(Value::list(xs[..take_n].to_vec()))
        },
    );
    interp.register_fn(
        "drop",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let n = expect_int(&args[0], sp)?.max(0) as usize;
            let xs = list_view(&args[1], sp)?;
            let drop_n = n.min(xs.len());
            Ok(Value::list(xs[drop_n..].to_vec()))
        },
    );
    interp.register_fn(
        "nth",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let n = expect_int(&args[0], sp)?;
            let xs = list_view(&args[1], sp)?;
            if n < 0 || (n as usize) >= xs.len() {
                Ok(Value::Nil)
            } else {
                Ok(xs[n as usize].clone())
            }
        },
    );
    interp.register_fn("not=", Arity::Exact(2), |a: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(!value_eq_deep(&a[0], &a[1])))
    });
    interp.register_fn(
        "atom?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| {
            Ok(Value::Bool(!matches!(
                &a[0],
                Value::List(_) | Value::Nil | Value::Closure(_) | Value::NativeFn(_)
            )))
        },
    );
    interp.register_fn(
        "keyword?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(Value::Bool(matches!(&a[0], Value::Keyword(_)))),
    );

    // ── String <-> symbol/keyword interop ────────────────────────
    interp.register_fn(
        "symbol->string",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Symbol(s) => Ok(Value::Str(s.clone())),
            other => Err(EvalError::type_mismatch("symbol", other.type_name(), sp)),
        },
    );
    interp.register_fn(
        "keyword->string",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Keyword(s) => Ok(Value::Str(s.clone())),
            other => Err(EvalError::type_mismatch("keyword", other.type_name(), sp)),
        },
    );
    interp.register_fn(
        "string->symbol",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Str(s) => Ok(Value::Symbol(s.clone())),
            other => Err(EvalError::type_mismatch("string", other.type_name(), sp)),
        },
    );
    interp.register_fn(
        "string->keyword",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| match &args[0] {
            Value::Str(s) => Ok(Value::Keyword(s.clone())),
            other => Err(EvalError::type_mismatch("string", other.type_name(), sp)),
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

    // ── gensym ────────────────────────────────────────────────────
    // Process-global counter — guaranteed unique across all
    // Interpreters in the same process. `(gensym)` returns "g42";
    // `(gensym "tag")` returns "tag42".
    interp.register_fn("gensym", Arity::Range(0, 1), |args: &[Value], _h: &mut H, sp| {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let prefix: String = match args.first() {
            None => "g".to_string(),
            Some(Value::Str(s)) => s.to_string(),
            Some(Value::Symbol(s)) => s.to_string(),
            Some(other) => return Err(EvalError::type_mismatch("string or symbol", other.type_name(), sp)),
        };
        Ok(Value::Symbol(Arc::from(format!("{prefix}__{n}__auto"))))
    });

    // ── Structured errors ─────────────────────────────────────────
    // Clojure-style ex-info: tag + message + data plist.
    use crate::value::ErrorObj;

    // (error tag message [data]) — tag is keyword/string, message is
    // string, data is an optional plist (alternating key value list).
    interp.register_fn("error", Arity::Range(2, 3), |args: &[Value], _h: &mut H, sp| {
        let tag: Arc<str> = match &args[0] {
            Value::Keyword(s) | Value::Symbol(s) | Value::Str(s) => s.clone(),
            other => return Err(EvalError::type_mismatch("keyword/symbol/string", other.type_name(), sp)),
        };
        let message: Arc<str> = match &args[1] {
            Value::Str(s) => s.clone(),
            other => return Err(EvalError::type_mismatch("string", other.type_name(), sp)),
        };
        let data = if args.len() == 3 {
            plist_to_pairs(&args[2], sp)?
        } else {
            Vec::new()
        };
        Ok(Value::Error(Arc::new(ErrorObj { tag, message, data })))
    });

    // (ex-info message data) — convenience: tag = "ex-info".
    interp.register_fn("ex-info", Arity::Range(1, 2), |args: &[Value], _h: &mut H, sp| {
        let message: Arc<str> = match &args[0] {
            Value::Str(s) => s.clone(),
            other => return Err(EvalError::type_mismatch("string", other.type_name(), sp)),
        };
        let data = if args.len() == 2 {
            plist_to_pairs(&args[1], sp)?
        } else {
            Vec::new()
        };
        Ok(Value::Error(Arc::new(ErrorObj {
            tag: Arc::from("ex-info"),
            message,
            data,
        })))
    });

    // (throw err) — raise as EvalError::User. If err isn't an Error
    // value, wrap it with tag "user".
    interp.register_fn("throw", Arity::Exact(1), |args: &[Value], _h: &mut H, sp| {
        let value = args[0].clone();
        Err(EvalError::User { value, at: sp })
    });

    // Predicate.
    interp.register_fn("error?", Arity::Exact(1), |args: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(matches!(&args[0], Value::Error(_))))
    });

    // Accessors.
    interp.register_fn("error-tag", Arity::Exact(1), |args: &[Value], _h: &mut H, sp| {
        match &args[0] {
            Value::Error(e) => Ok(Value::Keyword(e.tag.clone())),
            other => Err(EvalError::type_mismatch("error", other.type_name(), sp)),
        }
    });

    interp.register_fn("error-message", Arity::Exact(1), |args: &[Value], _h: &mut H, sp| {
        match &args[0] {
            Value::Error(e) => Ok(Value::Str(e.message.clone())),
            other => Err(EvalError::type_mismatch("error", other.type_name(), sp)),
        }
    });

    // (error-data err) → plist (k1 v1 k2 v2 ...).
    interp.register_fn("error-data", Arity::Exact(1), |args: &[Value], _h: &mut H, sp| {
        match &args[0] {
            Value::Error(e) => {
                let mut out = Vec::with_capacity(e.data.len() * 2);
                for (k, v) in &e.data {
                    out.push(k.clone());
                    out.push(v.clone());
                }
                Ok(Value::list(out))
            }
            other => Err(EvalError::type_mismatch("error", other.type_name(), sp)),
        }
    });

    // (error-data-get err key) → value or nil.
    interp.register_fn("error-data-get", Arity::Exact(2), |args: &[Value], _h: &mut H, sp| {
        match &args[0] {
            Value::Error(e) => {
                for (k, v) in &e.data {
                    if value_eq_deep(k, &args[1]) {
                        return Ok(v.clone());
                    }
                }
                Ok(Value::Nil)
            }
            other => Err(EvalError::type_mismatch("error", other.type_name(), sp)),
        }
    });
}

/// Parse a plist Value (alternating k/v list) into a `Vec<(Value, Value)>`.
fn plist_to_pairs(v: &Value, sp: Span) -> Result<Vec<(Value, Value)>> {
    let xs = match v {
        Value::Nil => return Ok(Vec::new()),
        Value::List(xs) => xs,
        other => return Err(EvalError::type_mismatch("plist (list)", other.type_name(), sp)),
    };
    if xs.len() % 2 != 0 {
        return Err(EvalError::native_fn(
            Arc::<str>::from("plist"),
            "plist must have even number of elements (k v k v ...)",
            sp,
        ));
    }
    let mut out = Vec::with_capacity(xs.len() / 2);
    let mut i = 0;
    while i < xs.len() {
        out.push((xs[i].clone(), xs[i + 1].clone()));
        i += 2;
    }
    Ok(out)
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Borrow a list-shaped Value as a slice. `Nil` is treated as an empty
/// list. Used by primitives that don't care about ownership.
fn list_view(v: &Value, sp: Span) -> Result<&[Value]> {
    match v {
        Value::Nil => Ok(&[]),
        Value::List(xs) => Ok(xs.as_ref()),
        other => Err(EvalError::type_mismatch("list", other.type_name(), sp)),
    }
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
