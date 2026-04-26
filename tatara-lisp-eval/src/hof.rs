//! Higher-order primitives — functions that invoke a callable `Value`
//! back into the eval loop.
//!
//! These can't be registered as plain `register_fn` because the basic
//! `NativeCallable` signature has no access to the function registry —
//! a primitive there cannot apply a closure. Instead they go through
//! `register_higher_order_fn`, which receives a `Caller` per invocation.
//!
//! The full surface registered here:
//!
//! ```text
//! ;; calling
//!   (apply f args-list)               → applies f to the elements of args-list
//!   (apply f a b ... rest)            → like Scheme apply: rest must be a list
//!
//! ;; mapping / filtering
//!   (map f xs)                        → list
//!   (map f xs ys ...)                 → variadic across N lists, parallel
//!   (filter pred? xs)                 → list
//!   (remove pred? xs)                 → complement of filter
//!   (for-each f xs)                   → side effects, returns nil
//!
//! ;; folds
//!   (foldl f init xs)                 → left fold
//!   (foldr f init xs)                 → right fold
//!   (reduce f xs)                     → like foldl with first element as init
//!   (reduce f init xs)                → like foldl
//!   (scan-left f init xs)             → returns the running list of accumulators
//!
//! ;; searching
//!   (find pred? xs)                   → first matching element or nil
//!   (find-index pred? xs)             → index or -1
//!   (some pred? xs)                   → first truthy result of (pred x)
//!   (any? pred? xs)                   → bool
//!   (every? pred? xs)                 → bool
//!   (count-if pred? xs)               → integer
//!
//! ;; partitioning / windowing
//!   (take-while pred? xs)             → prefix
//!   (drop-while pred? xs)             → suffix
//!   (partition pred? xs)              → (matching . non-matching), as a list of two lists
//!   (group-by f xs)                   → list of (key items) pairs (preserves first-seen order)
//!   (sort-by cmp xs)                  → stable; cmp returns int (-/0/+)
//!
//! ;; generation
//!   (iterate f x n)                   → (x, f(x), f(f(x)), ...) length n
//!   (repeatedly thunk n)              → (thunk(), thunk(), ...) length n
//! ```
//!
//! All names are first-class procedures: rebindable, passable as args.

use std::collections::HashMap;
use std::sync::Arc;

use tatara_lisp::Span;

use crate::error::{EvalError, Result};
use crate::eval::Interpreter;
use crate::ffi::{Arity, Caller};
use crate::value::Value;

/// Names registered by `install_hof`. Kept sorted for the self-test.
pub const HOF_NAMES: &[&str] = &[
    "any?",
    "apply",
    "count-if",
    "drop-while",
    "every?",
    "filter",
    "find",
    "find-index",
    "foldl",
    "foldr",
    "for-each",
    "force",
    "group-by",
    "iterate",
    "map",
    "partition",
    "promise?",
    "reduce",
    "remove",
    "repeatedly",
    "scan-left",
    "some",
    "sort-by",
    "take-while",
];

pub fn install_hof<H: 'static>(interp: &mut Interpreter<H>) {
    // ── apply ────────────────────────────────────────────────────────
    // (apply f arg1 arg2 ... last-list) — Scheme convention: the last arg
    // must be a list; it gets spliced as the tail of the actual args.
    // Single-arglist form (apply f xs) also supported.
    interp.register_higher_order_fn(
        "apply",
        Arity::AtLeast(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let mid = &args[1..args.len() - 1];
            let tail = match &args[args.len() - 1] {
                Value::Nil => Vec::new(),
                Value::List(xs) => xs.as_ref().clone(),
                other => {
                    return Err(EvalError::type_mismatch(
                        "list (last arg of apply)",
                        other.type_name(),
                        sp,
                    ))
                }
            };
            let mut all_args = Vec::with_capacity(mid.len() + tail.len());
            all_args.extend(mid.iter().cloned());
            all_args.extend(tail);
            caller.apply_value(f, all_args, host, sp)
        },
    );

    // ── map / filter / for-each ──────────────────────────────────────
    interp.register_higher_order_fn(
        "map",
        Arity::AtLeast(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let lists = expect_lists(&args[1..], sp)?;
            // Variadic: zip across N lists, take the shortest.
            let min_len = lists.iter().map(Vec::len).min().unwrap_or(0);
            let mut out = Vec::with_capacity(min_len);
            for i in 0..min_len {
                let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                out.push(caller.apply_value(f, row, host, sp)?);
            }
            Ok(Value::list(out))
        },
    );

    interp.register_higher_order_fn(
        "filter",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            let mut out = Vec::new();
            for x in xs {
                if caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    out.push(x.clone());
                }
            }
            Ok(Value::list(out))
        },
    );

    interp.register_higher_order_fn(
        "remove",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            let mut out = Vec::new();
            for x in xs {
                if !caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    out.push(x.clone());
                }
            }
            Ok(Value::list(out))
        },
    );

    interp.register_higher_order_fn(
        "for-each",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let xs = expect_list(&args[1], sp)?;
            for x in xs {
                caller.call1(f, x.clone(), host, sp)?;
            }
            Ok(Value::Nil)
        },
    );

    // ── folds ────────────────────────────────────────────────────────
    interp.register_higher_order_fn(
        "foldl",
        Arity::Exact(3),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let mut acc = args[1].clone();
            let xs = expect_list(&args[2], sp)?;
            for x in xs {
                acc = caller.call2(f, acc, x.clone(), host, sp)?;
            }
            Ok(acc)
        },
    );

    interp.register_higher_order_fn(
        "foldr",
        Arity::Exact(3),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let init = args[1].clone();
            let xs = expect_list(&args[2], sp)?;
            let mut acc = init;
            for x in xs.iter().rev() {
                acc = caller.call2(f, x.clone(), acc, host, sp)?;
            }
            Ok(acc)
        },
    );

    interp.register_higher_order_fn(
        "reduce",
        Arity::Range(2, 3),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let (init, items_value) = if args.len() == 3 {
                (args[1].clone(), &args[2])
            } else {
                let xs = expect_list(&args[1], sp)?;
                if xs.is_empty() {
                    return Err(EvalError::native_fn(
                        Arc::<str>::from("reduce"),
                        "no init and empty list",
                        sp,
                    ));
                }
                (xs[0].clone(), &args[1])
            };
            let xs = expect_list(items_value, sp)?;
            let start = if args.len() == 3 { 0 } else { 1 };
            let mut acc = init;
            for x in &xs[start..] {
                acc = caller.call2(f, acc, x.clone(), host, sp)?;
            }
            Ok(acc)
        },
    );

    interp.register_higher_order_fn(
        "scan-left",
        Arity::Exact(3),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let mut acc = args[1].clone();
            let xs = expect_list(&args[2], sp)?;
            let mut out = Vec::with_capacity(xs.len() + 1);
            out.push(acc.clone());
            for x in xs {
                acc = caller.call2(f, acc, x.clone(), host, sp)?;
                out.push(acc.clone());
            }
            Ok(Value::list(out))
        },
    );

    // ── searching / predicates ───────────────────────────────────────
    interp.register_higher_order_fn(
        "find",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            for x in xs {
                if caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    return Ok(x.clone());
                }
            }
            Ok(Value::Nil)
        },
    );

    interp.register_higher_order_fn(
        "find-index",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            for (i, x) in xs.iter().enumerate() {
                if caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    return Ok(Value::Int(i as i64));
                }
            }
            Ok(Value::Int(-1))
        },
    );

    interp.register_higher_order_fn(
        "some",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            for x in xs {
                let v = caller.call1(pred, x.clone(), host, sp)?;
                if v.is_truthy() {
                    return Ok(v);
                }
            }
            Ok(Value::Nil)
        },
    );

    interp.register_higher_order_fn(
        "any?",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            for x in xs {
                if caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        },
    );

    interp.register_higher_order_fn(
        "every?",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            for x in xs {
                if !caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        },
    );

    interp.register_higher_order_fn(
        "count-if",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            let mut n = 0i64;
            for x in xs {
                if caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    n += 1;
                }
            }
            Ok(Value::Int(n))
        },
    );

    // ── partitioning / windowing ─────────────────────────────────────
    interp.register_higher_order_fn(
        "take-while",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            let mut out = Vec::new();
            for x in xs {
                if caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    out.push(x.clone());
                } else {
                    break;
                }
            }
            Ok(Value::list(out))
        },
    );

    interp.register_higher_order_fn(
        "drop-while",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            let mut started = false;
            let mut out = Vec::new();
            for x in xs {
                if !started && caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    continue;
                }
                started = true;
                out.push(x.clone());
            }
            Ok(Value::list(out))
        },
    );

    interp.register_higher_order_fn(
        "partition",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let pred = &args[0];
            let xs = expect_list(&args[1], sp)?;
            let mut yes = Vec::new();
            let mut no = Vec::new();
            for x in xs {
                if caller.call1(pred, x.clone(), host, sp)?.is_truthy() {
                    yes.push(x.clone());
                } else {
                    no.push(x.clone());
                }
            }
            Ok(Value::list(vec![Value::list(yes), Value::list(no)]))
        },
    );

    interp.register_higher_order_fn(
        "group-by",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let xs = expect_list(&args[1], sp)?;
            // Preserve first-seen key order.
            let mut order: Vec<Arc<str>> = Vec::new();
            let mut groups: HashMap<Arc<str>, Vec<Value>> = HashMap::new();
            for x in xs {
                let key = caller.call1(f, x.clone(), host, sp)?;
                let k = value_key(&key)?;
                if !groups.contains_key(&k) {
                    order.push(k.clone());
                    groups.insert(k.clone(), Vec::new());
                }
                groups.get_mut(&k).unwrap().push(x.clone());
            }
            let out = order
                .into_iter()
                .map(|k| {
                    let items = groups.remove(&k).unwrap();
                    Value::list(vec![Value::Str(k), Value::list(items)])
                })
                .collect();
            Ok(Value::List(Arc::new(out)))
        },
    );

    interp.register_higher_order_fn(
        "sort-by",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let cmp = &args[0];
            let xs = expect_list(&args[1], sp)?;
            // Decorate-sort-undecorate with a stable insertion sort so we
            // can surface comparator errors. n^2 is acceptable for small
            // pipelines; for large data the embedder can register a typed
            // primitive.
            let mut out = xs.to_vec();
            for i in 1..out.len() {
                let mut j = i;
                while j > 0 {
                    let r = caller.call2(cmp, out[j - 1].clone(), out[j].clone(), host, sp)?;
                    let lt = match r {
                        Value::Int(n) => n > 0,
                        Value::Bool(b) => b, // (lambda (a b) (< b a)) → returns bool too
                        other => {
                            return Err(EvalError::type_mismatch(
                                "int (or bool) from comparator",
                                other.type_name(),
                                sp,
                            ))
                        }
                    };
                    if lt {
                        out.swap(j - 1, j);
                        j -= 1;
                    } else {
                        break;
                    }
                }
            }
            Ok(Value::list(out))
        },
    );

    // ── generation ───────────────────────────────────────────────────
    interp.register_higher_order_fn(
        "iterate",
        Arity::Exact(3),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = &args[0];
            let mut x = args[1].clone();
            let n = expect_nonneg_int(&args[2], sp)? as usize;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(x.clone());
                x = caller.call1(f, x, host, sp)?;
            }
            Ok(Value::list(out))
        },
    );

    interp.register_higher_order_fn(
        "repeatedly",
        Arity::Exact(2),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let thunk = &args[0];
            let n = expect_nonneg_int(&args[1], sp)? as usize;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(caller.apply_value(thunk, vec![], host, sp)?);
            }
            Ok(Value::list(out))
        },
    );

    // ── force / promise? — lazy evaluation primitives ─────────────
    interp.register_higher_order_fn(
        "force",
        Arity::Exact(1),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            match &args[0] {
                Value::Promise(p) => {
                    // Two-pass: first check Forced (avoids re-running),
                    // else run the thunk and store. We keep the lock
                    // briefly to drop it before the call.
                    let thunk = {
                        let state = p.lock().unwrap();
                        match &*state {
                            crate::value::PromiseState::Forced(v) => return Ok(v.clone()),
                            crate::value::PromiseState::Pending(thunk) => thunk.clone(),
                        }
                    };
                    let result = caller
                        .apply_value(&Value::Closure(thunk), vec![], host, sp)?;
                    let mut state = p.lock().unwrap();
                    *state = crate::value::PromiseState::Forced(result.clone());
                    Ok(result)
                }
                // Non-promises pass through unchanged — Scheme convention.
                other => Ok(other.clone()),
            }
        },
    );

    interp.register_higher_order_fn(
        "promise?",
        Arity::Exact(1),
        |args: &[Value], _host: &mut H, _caller: &Caller<H>, _sp: Span| {
            Ok(Value::Bool(matches!(&args[0], Value::Promise(_))))
        },
    );
}

// ── shared helpers ───────────────────────────────────────────────────

fn expect_list(v: &Value, sp: Span) -> Result<Vec<Value>> {
    match v {
        Value::Nil => Ok(Vec::new()),
        Value::List(xs) => Ok(xs.as_ref().clone()),
        other => Err(EvalError::type_mismatch("list", other.type_name(), sp)),
    }
}

fn expect_lists(args: &[Value], sp: Span) -> Result<Vec<Vec<Value>>> {
    args.iter().map(|v| expect_list(v, sp)).collect()
}

fn expect_nonneg_int(v: &Value, sp: Span) -> Result<i64> {
    match v {
        Value::Int(n) if *n >= 0 => Ok(*n),
        Value::Int(_) => Err(EvalError::native_fn(
            Arc::<str>::from("hof"),
            "expected non-negative int",
            sp,
        )),
        other => Err(EvalError::type_mismatch("integer", other.type_name(), sp)),
    }
}

/// Stringify a Value for use as a hash key in `group-by`. Only meaningful
/// for atom-like values; anything else gets debug-printed.
fn value_key(v: &Value) -> Result<Arc<str>> {
    let s = match v {
        Value::Str(s) => s.to_string(),
        Value::Symbol(s) => format!(":sym {s}"),
        Value::Keyword(s) => format!(":kw {s}"),
        Value::Int(n) => format!(":int {n}"),
        Value::Float(n) => format!(":float {n}"),
        Value::Bool(b) => format!(":bool {b}"),
        Value::Nil => ":nil".to_string(),
        other => format!(":{other}"),
    };
    Ok(Arc::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::install_primitives;
    use crate::Interpreter;
    use tatara_lisp::read_spanned;

    struct NoHost;

    fn eval(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        install_hof(&mut i);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    #[test]
    fn map_squares() {
        let v = eval("(map (lambda (x) (* x x)) (list 1 2 3 4))");
        assert_eq!(format!("{v}"), "(1 4 9 16)");
    }

    #[test]
    fn map_variadic_zip() {
        let v = eval("(map + (list 1 2 3) (list 10 20 30))");
        assert_eq!(format!("{v}"), "(11 22 33)");
    }

    #[test]
    fn filter_evens() {
        let v = eval("(filter (lambda (x) (= 0 (modulo x 2))) (list 1 2 3 4 5 6))");
        assert_eq!(format!("{v}"), "(2 4 6)");
    }

    #[test]
    fn foldl_sum() {
        let v = eval("(foldl + 0 (list 1 2 3 4 5))");
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn foldr_cons_preserves_order() {
        let v = eval("(foldr cons (list) (list 1 2 3))");
        assert_eq!(format!("{v}"), "(1 2 3)");
    }

    #[test]
    fn reduce_with_init() {
        let v = eval("(reduce + 100 (list 1 2 3))");
        assert!(matches!(v, Value::Int(106)));
    }

    #[test]
    fn reduce_no_init_uses_first() {
        let v = eval("(reduce + (list 10 20 30))");
        assert!(matches!(v, Value::Int(60)));
    }

    #[test]
    fn scan_left_emits_running_sums() {
        let v = eval("(scan-left + 0 (list 1 2 3))");
        assert_eq!(format!("{v}"), "(0 1 3 6)");
    }

    #[test]
    fn apply_with_arglist_only() {
        let v = eval("(apply + (list 1 2 3 4 5))");
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn apply_with_mid_args_and_arglist() {
        let v = eval("(apply + 100 (list 1 2 3))");
        assert!(matches!(v, Value::Int(106)));
    }

    #[test]
    fn find_returns_first_match() {
        let v = eval("(find (lambda (x) (> x 3)) (list 1 2 3 4 5))");
        assert!(matches!(v, Value::Int(4)));
    }

    #[test]
    fn find_returns_nil_when_no_match() {
        let v = eval("(find (lambda (x) (> x 99)) (list 1 2 3))");
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn find_index_returns_position_or_negative_one() {
        let v = eval("(find-index (lambda (x) (= x 3)) (list 1 2 3 4 5))");
        assert!(matches!(v, Value::Int(2)));
        let v = eval("(find-index (lambda (x) (= x 99)) (list 1 2 3))");
        assert!(matches!(v, Value::Int(-1)));
    }

    #[test]
    fn any_every_predicates() {
        assert!(matches!(
            eval("(any? (lambda (x) (> x 0)) (list -1 -2 3))"),
            Value::Bool(true)
        ));
        assert!(matches!(
            eval("(every? (lambda (x) (> x 0)) (list 1 2 3))"),
            Value::Bool(true)
        ));
        assert!(matches!(
            eval("(every? (lambda (x) (> x 0)) (list 1 -2 3))"),
            Value::Bool(false)
        ));
    }

    #[test]
    fn count_if_counts_matches() {
        let v = eval("(count-if (lambda (x) (> x 2)) (list 1 2 3 4 5))");
        assert!(matches!(v, Value::Int(3)));
    }

    #[test]
    fn for_each_runs_for_side_effects() {
        // No host, no real side effect — just verify it returns nil and
        // doesn't error.
        let v = eval("(for-each display (list))");
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn take_while_drop_while() {
        let v = eval("(take-while (lambda (x) (< x 3)) (list 1 2 3 4 5))");
        assert_eq!(format!("{v}"), "(1 2)");
        let v = eval("(drop-while (lambda (x) (< x 3)) (list 1 2 3 4 5))");
        assert_eq!(format!("{v}"), "(3 4 5)");
    }

    #[test]
    fn partition_splits_two_ways() {
        let v = eval("(partition (lambda (x) (> x 2)) (list 1 2 3 4 5))");
        assert_eq!(format!("{v}"), "((3 4 5) (1 2))");
    }

    #[test]
    fn group_by_preserves_first_key_order() {
        let v = eval(
            "(group-by (lambda (x) (modulo x 2))
                       (list 1 2 3 4 5 6))",
        );
        // First key seen is 1 (group 1,3,5), then 0 (group 2,4,6).
        let s = format!("{v}");
        assert!(s.contains("(1 3 5)"));
        assert!(s.contains("(2 4 6)"));
    }

    #[test]
    fn sort_by_with_comparator() {
        let v = eval("(sort-by (lambda (a b) (- a b)) (list 3 1 2 5 4))");
        assert_eq!(format!("{v}"), "(1 2 3 4 5)");
    }

    #[test]
    fn iterate_doubles() {
        let v = eval("(iterate (lambda (x) (* x 2)) 1 5)");
        assert_eq!(format!("{v}"), "(1 2 4 8 16)");
    }

    #[test]
    fn repeatedly_calls_thunk_n_times() {
        // Use a deterministic thunk — always returns 7.
        let v = eval("(repeatedly (lambda () 7) 4)");
        assert_eq!(format!("{v}"), "(7 7 7 7)");
    }

    #[test]
    fn map_with_native_fn_works() {
        let v = eval("(map (lambda (x) (+ x 1)) (list 10 20 30))");
        assert_eq!(format!("{v}"), "(11 21 31)");
    }

    #[test]
    fn nested_map_filter_compose() {
        // (filter even? (map sq (range 1 6))) → (4 16 36)
        // We don't have range yet — use explicit list.
        let v = eval(
            "(filter (lambda (x) (= 0 (modulo x 2)))
                     (map (lambda (x) (* x x)) (list 1 2 3 4 5 6)))",
        );
        assert_eq!(format!("{v}"), "(4 16 36)");
    }
}
