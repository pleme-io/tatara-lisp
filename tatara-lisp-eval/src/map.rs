//! Hash-map primitives — `Value::Map` operations.
//!
//! Maps are persistent (copy-on-write through `Arc<HashMap>`). Keys
//! must be hashable: `Bool`, `Int`, `Float`, `Str`, `Symbol`, `Keyword`,
//! `Nil`. Inserting a non-hashable key raises a TypeMismatch with a
//! message naming the key kind.
//!
//! Surface:
//!
//! ```text
//!   (hash-map)                 → empty map
//!   (hash-map k v ...)         → map with given pairs (variadic)
//!   (hash-map? v)              → bool
//!   (hash-map-count m)         → int
//!   (hash-map-empty? m)        → bool
//!   (hash-map-has? m k)        → bool
//!   (hash-map-get m k)         → value or nil
//!   (hash-map-get-or m k def)  → value or default
//!   (hash-map-set m k v)       → new map with k→v
//!   (hash-map-remove m k)      → new map without k
//!   (hash-map-keys m)          → list of keys
//!   (hash-map-values m)        → list of values
//!   (hash-map-entries m)       → list of (k v) pairs
//!   (hash-map-merge m1 m2 ...) → merged; later overrides earlier
//!   (hash-map-update m k fn)   → set k to (fn current-or-nil)
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use tatara_lisp::Span;

use crate::error::{EvalError, Result};
use crate::eval::Interpreter;
use crate::ffi::{Arity, Caller};
use crate::value::{MapKey, Value};

/// Names registered. Kept sorted for the self-test.
pub const MAP_NAMES: &[&str] = &[
    "hash-map",
    "hash-map-count",
    "hash-map-empty?",
    "hash-map-entries",
    "hash-map-get",
    "hash-map-get-or",
    "hash-map-has?",
    "hash-map-keys",
    "hash-map-merge",
    "hash-map-remove",
    "hash-map-set",
    "hash-map-update",
    "hash-map-values",
    "hash-map?",
];

pub fn install_map<H: 'static>(interp: &mut Interpreter<H>) {
    interp.register_fn(
        "hash-map",
        Arity::Any,
        |args: &[Value], _h: &mut H, sp: Span| {
            if args.len() % 2 != 0 {
                return Err(EvalError::native_fn(
                    Arc::<str>::from("hash-map"),
                    "expected even number of args (k v k v ...)",
                    sp,
                ));
            }
            let mut m = HashMap::with_capacity(args.len() / 2);
            let mut i = 0;
            while i < args.len() {
                let k = key_or_err(&args[i], sp)?;
                m.insert(k, args[i + 1].clone());
                i += 2;
            }
            Ok(Value::Map(Arc::new(m)))
        },
    );

    interp.register_fn(
        "hash-map?",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, _sp| Ok(Value::Bool(matches!(&args[0], Value::Map(_)))),
    );

    interp.register_fn(
        "hash-map-count",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            Ok(Value::Int(m.len() as i64))
        },
    );

    interp.register_fn(
        "hash-map-empty?",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            Ok(Value::Bool(m.is_empty()))
        },
    );

    interp.register_fn(
        "hash-map-has?",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let k = key_or_err(&args[1], sp)?;
            Ok(Value::Bool(m.contains_key(&k)))
        },
    );

    interp.register_fn(
        "hash-map-get",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let k = key_or_err(&args[1], sp)?;
            Ok(m.get(&k).cloned().unwrap_or(Value::Nil))
        },
    );

    interp.register_fn(
        "hash-map-get-or",
        Arity::Exact(3),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let k = key_or_err(&args[1], sp)?;
            Ok(m.get(&k).cloned().unwrap_or_else(|| args[2].clone()))
        },
    );

    interp.register_fn(
        "hash-map-set",
        Arity::Exact(3),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let k = key_or_err(&args[1], sp)?;
            let mut copy = m.as_ref().clone();
            copy.insert(k, args[2].clone());
            Ok(Value::Map(Arc::new(copy)))
        },
    );

    interp.register_fn(
        "hash-map-remove",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let k = key_or_err(&args[1], sp)?;
            let mut copy = m.as_ref().clone();
            copy.remove(&k);
            Ok(Value::Map(Arc::new(copy)))
        },
    );

    interp.register_fn(
        "hash-map-keys",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let keys: Vec<Value> = m.keys().map(MapKey::to_value).collect();
            Ok(Value::list(keys))
        },
    );

    interp.register_fn(
        "hash-map-values",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let vs: Vec<Value> = m.values().cloned().collect();
            Ok(Value::list(vs))
        },
    );

    interp.register_fn(
        "hash-map-entries",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let m = expect_map(&args[0], sp)?;
            let entries: Vec<Value> = m
                .iter()
                .map(|(k, v)| Value::list(vec![k.to_value(), v.clone()]))
                .collect();
            Ok(Value::list(entries))
        },
    );

    interp.register_fn(
        "hash-map-merge",
        Arity::AtLeast(1),
        |args: &[Value], _h: &mut H, sp| {
            let mut acc = expect_map(&args[0], sp)?.as_ref().clone();
            for arg in &args[1..] {
                let other = expect_map(arg, sp)?;
                for (k, v) in other.iter() {
                    acc.insert(k.clone(), v.clone());
                }
            }
            Ok(Value::Map(Arc::new(acc)))
        },
    );

    // Higher-order: update via callback. (hash-map-update m k fn) →
    // map with k bound to (fn current-or-nil). Needs Caller because
    // fn is a callable Value.
    interp.register_higher_order_fn(
        "hash-map-update",
        Arity::Exact(3),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let m = expect_map(&args[0], sp)?;
            let k = key_or_err(&args[1], sp)?;
            let f = &args[2];
            let current = m.get(&k).cloned().unwrap_or(Value::Nil);
            let new_v = caller.call1(f, current, host, sp)?;
            let mut copy = m.as_ref().clone();
            copy.insert(k, new_v);
            Ok(Value::Map(Arc::new(copy)))
        },
    );
}

fn expect_map(v: &Value, sp: Span) -> Result<Arc<HashMap<MapKey, Value>>> {
    match v {
        Value::Map(m) => Ok(m.clone()),
        other => Err(EvalError::type_mismatch("map", other.type_name(), sp)),
    }
}

fn key_or_err(v: &Value, sp: Span) -> Result<MapKey> {
    MapKey::from_value(v).ok_or_else(|| {
        EvalError::native_fn(
            Arc::<str>::from("hash-map"),
            format!("non-hashable key kind: {}", v.type_name()),
            sp,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::install_primitives;
    use crate::Interpreter;
    use tatara_lisp::read_spanned;

    struct NoHost;

    fn run(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        install_map(&mut i);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    #[test]
    fn hash_map_constructor() {
        let v = run("(hash-map :a 1 :b 2)");
        match v {
            Value::Map(m) => assert_eq!(m.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hash_map_get_returns_value_or_nil() {
        let v = run("(hash-map-get (hash-map :a 1 :b 2) :a)");
        assert!(matches!(v, Value::Int(1)));
        let v = run("(hash-map-get (hash-map :a 1) :missing)");
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn hash_map_get_or_default() {
        let v = run("(hash-map-get-or (hash-map) :missing 99)");
        assert!(matches!(v, Value::Int(99)));
    }

    #[test]
    fn hash_map_set_returns_new_map() {
        let v = run(
            "(let* ((m1 (hash-map :a 1))
                    (m2 (hash-map-set m1 :b 2)))
               (list (hash-map-count m1) (hash-map-count m2)))",
        );
        // m1 unchanged at 1 entry, m2 has 2.
        assert_eq!(format!("{v}"), "(1 2)");
    }

    #[test]
    fn hash_map_remove_returns_new_map_without_key() {
        let v = run("(hash-map-count (hash-map-remove (hash-map :a 1 :b 2) :a))");
        assert!(matches!(v, Value::Int(1)));
    }

    #[test]
    fn hash_map_has_predicate() {
        assert!(matches!(
            run("(hash-map-has? (hash-map :a 1) :a)"),
            Value::Bool(true)
        ));
        assert!(matches!(
            run("(hash-map-has? (hash-map :a 1) :b)"),
            Value::Bool(false)
        ));
    }

    #[test]
    fn hash_map_predicate_distinguishes() {
        assert!(matches!(
            run("(hash-map? (hash-map))"),
            Value::Bool(true)
        ));
        assert!(matches!(run("(hash-map? (list))"), Value::Bool(false)));
    }

    #[test]
    fn hash_map_keys_and_values() {
        // Order isn't guaranteed; check membership.
        let v = run("(hash-map-keys (hash-map :a 1 :b 2 :c 3))");
        let s = format!("{v}");
        assert!(s.contains(":a") && s.contains(":b") && s.contains(":c"));
    }

    #[test]
    fn hash_map_merge_later_wins() {
        let v = run(
            "(hash-map-get (hash-map-merge (hash-map :a 1) (hash-map :a 2)) :a)",
        );
        assert!(matches!(v, Value::Int(2)));
    }

    #[test]
    fn hash_map_update_via_callback() {
        let v = run(
            "(hash-map-get
               (hash-map-update (hash-map :n 5) :n (lambda (x) (* x x)))
               :n)",
        );
        assert!(matches!(v, Value::Int(25)));
    }

    #[test]
    fn hash_map_update_handles_missing_key() {
        // Missing key → fn receives nil. Lambda must handle it.
        let v = run(
            "(hash-map-get
               (hash-map-update (hash-map) :counter (lambda (x) (if (null? x) 1 (+ x 1))))
               :counter)",
        );
        assert!(matches!(v, Value::Int(1)));
    }

    #[test]
    fn hash_map_with_string_keys() {
        let v = run("(hash-map-get (hash-map \"name\" \"luis\") \"name\")");
        assert_eq!(format!("{v}"), "\"luis\"");
    }

    #[test]
    fn hash_map_with_int_keys() {
        let v = run("(hash-map-get (hash-map 42 :answer) 42)");
        assert!(matches!(v, Value::Keyword(s) if &*s == "answer"));
    }

    #[test]
    fn hash_map_non_hashable_key_errors() {
        // List as key — not hashable.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        install_map(&mut i);
        let forms = read_spanned("(hash-map (list 1 2) :v)").unwrap();
        let err = i.eval_program(&forms, &mut NoHost).unwrap_err();
        match err {
            EvalError::NativeFn { reason, .. } => assert!(reason.contains("non-hashable")),
            other => panic!("{other:?}"),
        }
    }
}
