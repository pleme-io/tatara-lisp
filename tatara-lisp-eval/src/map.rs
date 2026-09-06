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

    // The three map-rebuilding primitives take their arguments BY VALUE
    // (`register_owned_fn`), which is what lets `map_cow` reach an unaliased
    // map through `Arc::get_mut`. Everything else in this module only reads
    // its arguments and stays on the borrowed path.
    interp.register_owned_fn(
        "hash-map-set",
        Arity::Exact(3),
        |args: Vec<Value>, _h: &mut H, sp| {
            let [m, k, v] = owned_args::<3>("hash-map-set", args, sp)?;
            let k = key_or_err(&k, sp)?;
            map_cow(m, sp, move |map| {
                map.insert(k, v);
            })
        },
    );

    interp.register_owned_fn(
        "hash-map-remove",
        Arity::Exact(2),
        |args: Vec<Value>, _h: &mut H, sp| {
            let [m, k] = owned_args::<2>("hash-map-remove", args, sp)?;
            let k = key_or_err(&k, sp)?;
            map_cow(m, sp, move |map| {
                map.remove(&k);
            })
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

    interp.register_owned_fn(
        "hash-map-merge",
        Arity::AtLeast(1),
        |args: Vec<Value>, _h: &mut H, sp| {
            let mut args = args.into_iter();
            let first = args.next().ok_or_else(|| {
                EvalError::native_fn(
                    Arc::<str>::from("hash-map-merge"),
                    "expected at least 1 arg",
                    sp,
                )
            })?;
            // Type-check every operand before consuming the accumulator, so a
            // bad third argument cannot leave the first one half-merged.
            let rest = args
                .map(|arg| match arg {
                    Value::Map(m) => Ok(m),
                    other => Err(EvalError::type_mismatch("map", other.type_name(), sp)),
                })
                .collect::<Result<Vec<_>>>()?;
            map_cow(first, sp, move |acc| {
                for other in rest {
                    // An operand we hold alone is drained rather than copied;
                    // the same reading `map_cow` makes about the accumulator.
                    match Arc::try_unwrap(other) {
                        Ok(owned) => acc.extend(owned),
                        Err(shared) => {
                            acc.extend(shared.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                    }
                }
            })
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

/// Update a map, in place when the caller holds it alone.
///
/// **The one place the copy-on-write decision is made** — every map-rebuilding
/// primitive routes through here, so there is one shape to get right and one
/// shape to test.
///
/// `Arc::get_mut` answers the only question that matters: is this the sole
/// reference? If yes the update lands in the existing allocation and the
/// returned `Value::Map` names *the same* allocation — which is how the gate
/// in this module observes which branch ran, without asking the code under
/// test to report on itself. If no, the map is copied exactly once and the
/// original is left untouched, which is the semantics the language has always
/// promised.
///
/// This only reads honestly because the argument arrived by value. Under
/// [`crate::ffi::NativeCallable`]'s `&[Value]` the borrow is itself a second
/// reference, so `get_mut` returns `None` every time and the `else` branch is
/// the only reachable one.
fn map_cow<F>(v: Value, sp: Span, f: F) -> Result<Value>
where
    F: FnOnce(&mut HashMap<MapKey, Value>),
{
    match v {
        Value::Map(mut arc) => {
            if let Some(map) = Arc::get_mut(&mut arc) {
                f(map);
                Ok(Value::Map(arc))
            } else {
                let mut copy = HashMap::clone(&arc);
                drop(arc);
                f(&mut copy);
                Ok(Value::Map(Arc::new(copy)))
            }
        }
        other => Err(EvalError::type_mismatch("map", other.type_name(), sp)),
    }
}

/// Move a fixed-arity argument list out of the `Vec` the runtime handed us.
///
/// `apply` has already checked the arity against the registration, so the
/// error arm is unreachable through the interpreter — but it is a typed error
/// rather than a panic, because "unreachable" is a claim about a call path and
/// this function is reachable from anywhere in the crate.
fn owned_args<const N: usize>(who: &'static str, args: Vec<Value>, sp: Span) -> Result<[Value; N]> {
    let got = args.len();
    args.try_into().map_err(|_| {
        EvalError::native_fn(
            Arc::<str>::from(who),
            format!("expected exactly {N} args, got {got}"),
            sp,
        )
    })
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

    fn interp_with_maps() -> Interpreter<NoHost> {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        install_map(&mut i);
        i
    }

    fn run(src: &str) -> Value {
        let mut i = interp_with_maps();
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
        let v = run("(let* ((m1 (hash-map :a 1))
                    (m2 (hash-map-set m1 :b 2)))
               (list (hash-map-count m1) (hash-map-count m2)))");
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
        assert!(matches!(run("(hash-map? (hash-map))"), Value::Bool(true)));
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
        let v = run("(hash-map-get (hash-map-merge (hash-map :a 1) (hash-map :a 2)) :a)");
        assert!(matches!(v, Value::Int(2)));
    }

    #[test]
    fn hash_map_update_via_callback() {
        let v = run("(hash-map-get
               (hash-map-update (hash-map :n 5) :n (lambda (x) (* x x)))
               :n)");
        assert!(matches!(v, Value::Int(25)));
    }

    #[test]
    fn hash_map_update_handles_missing_key() {
        // Missing key → fn receives nil. Lambda must handle it.
        let v = run("(hash-map-get
               (hash-map-update (hash-map) :counter (lambda (x) (if (null? x) 1 (+ x 1))))
               :counter)");
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

    // ── The correctness half of the in-place gate ──────────────────────
    //
    // "Is the payload mutated in place?" is answered by allocation volume,
    // in `tests/owned_args.rs` — see the note there on why pointer identity
    // is NOT usable evidence. What lives here is the half that half can never
    // check: that a SHARED map is not disturbed. A gate that only measures
    // the fast path is satisfied by a primitive that always mutates, which is
    // a correctness bug, not an optimisation.

    fn sample_map(n: usize) -> Value {
        let mut m = HashMap::with_capacity(n);
        for k in 0..n {
            let k = i64::try_from(k).expect("fixture size fits i64");
            m.insert(MapKey::Int(k), Value::Int(k));
        }
        Value::Map(Arc::new(m))
    }

    /// Dispatch through the shipped call path rather than hand-calling the
    /// closure, so the gate measures what programs actually reach.
    fn dispatch(
        interp: &mut Interpreter<NoHost>,
        name: &str,
        arity: Arity,
        args: Vec<Value>,
    ) -> Value {
        let callee = Value::NativeFn(Arc::new(crate::value::NativeFn {
            name: Arc::from(name),
            arity,
        }));
        interp
            .apply_external_value(&callee, args, &mut NoHost, Span::synthetic())
            .unwrap()
    }

    fn len_of(v: &Value) -> usize {
        match v {
            Value::Map(m) => m.len(),
            other => panic!("{other:?}"),
        }
    }

    /// RED RUN 2026-08-13: `map_cow`'s copy arm rewritten to mutate through
    /// the shared `Arc` —
    ///   `let forced = Arc::as_ptr(&arc).cast_mut();`
    ///   `if let Some(map) = Some(unsafe { &mut *forced }) {`
    /// — so the primitive updates in place unconditionally. This test fails on
    /// the first assertion: `hash-map-set left a shared map mutated`,
    /// `left: 3, right: 2`. `a_named_binding_keeps_its_map` fails with it
    /// (`"(1 1 1)"` vs `"(1 2 1)"`), as does the pre-existing
    /// `hash_map_set_returns_new_map` (`"(2 2)"` vs `"(1 2)"`).
    ///
    /// Under the opposite mutation — the in-place arm disabled entirely — this
    /// test stays GREEN while the measurement in `tests/owned_args.rs` goes
    /// red. That asymmetry is the reason both halves exist.
    #[test]
    fn a_shared_map_is_never_mutated() {
        let mut i = interp_with_maps();

        let m = sample_map(2);
        let kept = m.clone();
        assert!(!m.is_unique(), "fixture must start aliased");
        let out = dispatch(
            &mut i,
            "hash-map-set",
            Arity::Exact(3),
            vec![m, Value::keyword("new"), Value::Int(9)],
        );
        assert_eq!(len_of(&kept), 2, "hash-map-set left a shared map mutated");
        assert_eq!(len_of(&out), 3);

        let m = sample_map(3);
        let kept = m.clone();
        let out = dispatch(
            &mut i,
            "hash-map-remove",
            Arity::Exact(2),
            vec![m, Value::Int(0)],
        );
        assert_eq!(
            len_of(&kept),
            3,
            "hash-map-remove left a shared map mutated"
        );
        assert_eq!(len_of(&out), 2);

        let a = sample_map(3);
        let kept = a.clone();
        let out = dispatch(
            &mut i,
            "hash-map-merge",
            Arity::AtLeast(1),
            vec![a, sample_map(4)],
        );
        assert_eq!(len_of(&kept), 3, "hash-map-merge left a shared map mutated");
        assert_eq!(len_of(&out), 4);
    }

    #[test]
    fn a_named_binding_keeps_its_map() {
        // The same property as the language sees it, through the real
        // pipeline: `m` is still reachable from its binding, so the set must
        // copy. This is the assertion that breaks first if the uniqueness
        // reading is ever wrong.
        let v = run("(let* ((m (hash-map :a 1))
                            (m2 (hash-map-set m :b 2))
                            (m3 (hash-map-remove m2 :a)))
                       (list (hash-map-count m) (hash-map-count m2) (hash-map-count m3)))");
        assert_eq!(format!("{v}"), "(1 2 1)");
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
