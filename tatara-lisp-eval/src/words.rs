//! Library words in Rust.
//!
//! These words were Lisp closures in `lisp_stdlib.tlisp`, so every call to one
//! was a full interpreted call, and a script calls `first`, `get`, `inc` or
//! `count` once per element. Each native keeps its Lisp definition's meaning:
//! nil against the empty list, Int against Float, the same errors with the same
//! messages (each error comes from the primitive the Lisp body called), and the
//! same arity. `tests/native_words.rs` holds every word against its Lisp twin
//! on a seeded corpus, and holds the list words linear at 200,000 elements.
//!
//! Three differences are deliberate. An arity error names the word, where a
//! closure named itself `<closure>`. A list word no longer recurses, so a long
//! list no longer exhausts the call depth. `range` and `repeat-list` refuse a
//! step that can never reach the end (a zero or NaN step, or a float that no
//! longer moves), where the Lisp recursion ran until the depth bound or memory
//! ran out.
//!
//! Higher-order words register through `register_higher_order_fn` and call
//! their function argument through [`Caller`], so a closure compiled by the VM
//! works as one built by the tree-walker.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use tatara_lisp::Span;

use crate::error::{EvalError, Result};
use crate::eval::Interpreter;
use crate::ffi::{Arity, Caller};
use crate::map::{key_or_err, map_get, map_has, map_new, map_remove, map_set};
use crate::persist::{List, Map};
use crate::primitive::{
    is_null, list_view, prim_add, prim_append, prim_car, prim_cdr, prim_error, prim_ge, prim_gt,
    prim_le, prim_lt, prim_modulo, prim_null, prim_num_eq, prim_sub, value_eq_deep,
};
use crate::value::{NativeFn, Value};

/// Names registered by [`install_words`]. Kept sorted for the self-test.
pub const WORD_NAMES: &[&str] = &[
    "assoc",
    "assoc-in",
    "butlast",
    "concat",
    "count",
    "dec",
    "dissoc",
    "distinct",
    "empty?",
    "even?",
    "first",
    "flatten",
    "fourth",
    "frequencies",
    "get",
    "get-in",
    "group-by-into-map",
    "identity",
    "inc",
    "interleave",
    "intersperse",
    "into-hash-map",
    "last",
    "lazy-drop",
    "lazy-empty",
    "lazy-empty?",
    "lazy-head",
    "lazy-rest",
    "lazy-take",
    "max-by",
    "member?",
    "min-by",
    "negative?",
    "not-empty?",
    "odd?",
    "plist-get",
    "position",
    "positive?",
    "range",
    "range-impl",
    "realize",
    "repeat-list",
    "rest",
    "safe-nth",
    "second",
    "select-keys",
    "some?",
    "tap",
    "third",
    "unwrap-or",
    "update-in",
    "zero?",
    "zip",
    "zipmap",
];

/// Register every word in [`WORD_NAMES`] on `interp`.
pub fn install_words<H: 'static>(interp: &mut Interpreter<H>) {
    install_list_words(interp);
    install_number_words(interp);
    install_lazy_words(interp);
    install_map_words(interp);
}

fn install_list_words<H: 'static>(interp: &mut Interpreter<H>) {
    interp.register_fn(
        "identity",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(a[0].clone()),
    );
    interp.register_higher_order_fn("tap", Arity::Exact(2), tap::<H>);

    interp.register_fn("first", Arity::Exact(1), prim_car::<H>);
    interp.register_fn("rest", Arity::Exact(1), prim_cdr::<H>);
    interp.register_fn("second", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        nth_by_cdr::<H>(&a[0], 1, h, sp)
    });
    interp.register_fn("third", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        nth_by_cdr::<H>(&a[0], 2, h, sp)
    });
    interp.register_fn("fourth", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        nth_by_cdr::<H>(&a[0], 3, h, sp)
    });
    interp.register_fn("last", Arity::Exact(1), last::<H>);
    interp.register_fn("butlast", Arity::Exact(1), butlast::<H>);
    interp.register_fn("safe-nth", Arity::Exact(2), safe_nth::<H>);
    interp.register_fn("empty?", Arity::Exact(1), prim_null::<H>);
    interp.register_fn(
        "not-empty?",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, _sp| Ok(Value::Bool(!is_null(&a[0]))),
    );

    interp.register_higher_order_fn("range", Arity::Any, range::<H>);
    interp.register_fn(
        "range-impl",
        Arity::Exact(3),
        |a: &[Value], h: &mut H, sp| range_impl::<H>(a[0].clone(), &a[1], &a[2], h, sp),
    );
    interp.register_fn("repeat-list", Arity::Exact(2), repeat_list::<H>);
    interp.register_owned_fn("concat", Arity::Any, prim_append::<H>);
    interp.register_fn("member?", Arity::Exact(2), |a: &[Value], _h: &mut H, sp| {
        Ok(Value::Bool(position_of(&a[0], &a[1], sp)?.is_some()))
    });
    interp.register_fn(
        "position",
        Arity::Exact(2),
        |a: &[Value], _h: &mut H, sp| {
            Ok(position_of(&a[0], &a[1], sp)?.map_or(Value::Int(-1), int_of))
        },
    );
    interp.register_fn("zip", Arity::Exact(2), |a: &[Value], _h: &mut H, sp| {
        Ok(Value::list(zip_pairs(&a[0], &a[1], sp)?))
    });
    interp.register_fn(
        "interleave",
        Arity::Exact(2),
        |a: &[Value], _h: &mut H, sp| {
            let pairs = zip_pairs(&a[0], &a[1], sp)?;
            let mut out = Vec::with_capacity(pairs.len() * 2);
            for pair in pairs {
                if let Value::List(xs) = pair {
                    out.extend(xs.iter().cloned());
                }
            }
            Ok(finish(out))
        },
    );
    interp.register_fn("intersperse", Arity::Exact(2), intersperse::<H>);
    interp.register_fn("flatten", Arity::Exact(1), flatten::<H>);
    interp.register_fn(
        "distinct",
        Arity::Exact(1),
        |a: &[Value], _h: &mut H, sp| distinct(&a[0], sp),
    );
    interp.register_higher_order_fn(
        "max-by",
        Arity::Exact(2),
        |a: &[Value], h: &mut H, c: &Caller<H>, sp| best_by::<H>(a, prim_gt::<H>, h, c, sp),
    );
    interp.register_higher_order_fn(
        "min-by",
        Arity::Exact(2),
        |a: &[Value], h: &mut H, c: &Caller<H>, sp| best_by::<H>(a, prim_lt::<H>, h, c, sp),
    );
}

fn install_number_words<H: 'static>(interp: &mut Interpreter<H>) {
    interp.register_fn("inc", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        prim_add::<H>(&[a[0].clone(), Value::Int(1)], h, sp)
    });
    interp.register_fn("dec", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        prim_sub::<H>(&[a[0].clone(), Value::Int(1)], h, sp)
    });
    interp.register_fn("zero?", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        prim_num_eq::<H>(&[a[0].clone(), Value::Int(0)], h, sp)
    });
    interp.register_fn(
        "positive?",
        Arity::Exact(1),
        |a: &[Value], h: &mut H, sp| prim_gt::<H>(&[a[0].clone(), Value::Int(0)], h, sp),
    );
    interp.register_fn(
        "negative?",
        Arity::Exact(1),
        |a: &[Value], h: &mut H, sp| prim_lt::<H>(&[a[0].clone(), Value::Int(0)], h, sp),
    );
    interp.register_fn("even?", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        parity::<H>(&a[0], 0, h, sp)
    });
    interp.register_fn("odd?", Arity::Exact(1), |a: &[Value], h: &mut H, sp| {
        parity::<H>(&a[0], 1, h, sp)
    });
    interp.register_fn("some?", Arity::Exact(1), |a: &[Value], _h: &mut H, _sp| {
        Ok(Value::Bool(!is_null(&a[0])))
    });
    interp.register_fn(
        "unwrap-or",
        Arity::Exact(2),
        |a: &[Value], _h: &mut H, _sp| {
            Ok(if is_null(&a[0]) {
                a[1].clone()
            } else {
                a[0].clone()
            })
        },
    );
    interp.register_fn("plist-get", Arity::Exact(2), plist_get::<H>);
}

fn install_lazy_words<H: 'static>(interp: &mut Interpreter<H>) {
    interp.register_fn(
        "lazy-empty",
        Arity::Exact(0),
        |_a: &[Value], _h: &mut H, _sp| Ok(Value::Nil),
    );
    interp.register_fn("lazy-empty?", Arity::Exact(1), prim_null::<H>);
    interp.register_fn("lazy-head", Arity::Exact(1), prim_car::<H>);
    interp.register_higher_order_fn(
        "lazy-rest",
        Arity::Exact(1),
        |a: &[Value], h: &mut H, c: &Caller<H>, sp| lazy_rest::<H>(&a[0], h, c, sp),
    );
    interp.register_higher_order_fn("lazy-take", Arity::Exact(2), lazy_take::<H>);
    interp.register_higher_order_fn("lazy-drop", Arity::Exact(2), lazy_drop::<H>);
    interp.register_higher_order_fn("realize", Arity::Exact(1), realize::<H>);
}

fn install_map_words<H: 'static>(interp: &mut Interpreter<H>) {
    interp.register_owned_fn("assoc", Arity::Exact(3), map_set::<H>);
    interp.register_owned_fn("dissoc", Arity::Exact(2), map_remove::<H>);
    interp.register_fn("get", Arity::Exact(2), map_get::<H>);
    interp.register_fn("get-in", Arity::Exact(2), |a: &[Value], h: &mut H, sp| {
        get_in::<H>(&a[0], &a[1], h, sp)
    });
    interp.register_fn("assoc-in", Arity::Exact(3), |a: &[Value], h: &mut H, sp| {
        assoc_in::<H>(&a[0], &a[1], a[2].clone(), h, sp)
    });
    interp.register_higher_order_fn(
        "update-in",
        Arity::Exact(3),
        |a: &[Value], h: &mut H, c: &Caller<H>, sp| {
            let current = get_in::<H>(&a[0], &a[1], h, sp)?;
            let next = c.call1(&a[2], current, h, sp)?;
            assoc_in::<H>(&a[0], &a[1], next, h, sp)
        },
    );
    interp.register_fn("frequencies", Arity::Exact(1), frequencies::<H>);
    interp.register_higher_order_fn("group-by-into-map", Arity::Exact(2), group_by_into_map::<H>);
    interp.register_fn("into-hash-map", Arity::Exact(1), into_hash_map::<H>);
    interp.register_fn("select-keys", Arity::Exact(2), select_keys::<H>);
    interp.register_fn("zipmap", Arity::Exact(2), |a: &[Value], _h: &mut H, sp| {
        let mut out = Map::new();
        for pair in zip_pairs(&a[0], &a[1], sp)? {
            if let Value::List(kv) = pair {
                out.insert(key_or_err(&kv[0], sp)?, kv[1].clone());
            }
        }
        Ok(Value::Map(Arc::new(out)))
    });
    interp.register_fn("count", Arity::Exact(1), count::<H>);
}

fn car<H: 'static>(v: &Value, h: &mut H, sp: Span) -> Result<Value> {
    prim_car::<H>(std::slice::from_ref(v), h, sp)
}

fn cdr<H: 'static>(v: &Value, h: &mut H, sp: Span) -> Result<Value> {
    prim_cdr::<H>(std::slice::from_ref(v), h, sp)
}

fn holds<H: 'static>(
    test: fn(&[Value], &mut H, Span) -> Result<Value>,
    a: &Value,
    b: Value,
    h: &mut H,
    sp: Span,
) -> Result<bool> {
    Ok(test(&[a.clone(), b], h, sp)?.is_truthy())
}

fn int_of(n: usize) -> Value {
    Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

fn finish(out: Vec<Value>) -> Value {
    if out.is_empty() {
        Value::Nil
    } else {
        Value::list(out)
    }
}

fn native(name: &str, arity: Arity) -> Value {
    Value::NativeFn(Arc::new(NativeFn {
        name: Arc::from(name),
        arity,
    }))
}

fn no_progress(word: &str, sp: Span) -> EvalError {
    EvalError::native_fn(word, "the step never reaches the end", sp)
}

fn stalled(before: &Value, after: &Value) -> bool {
    match (before, after) {
        (_, Value::Float(f)) if f.is_nan() => true,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a.total_cmp(b).is_eq(),
        _ => false,
    }
}

fn tap<H: 'static>(a: &[Value], h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    c.call1(&a[0], a[1].clone(), h, sp)?;
    Ok(a[1].clone())
}

fn nth_by_cdr<H: 'static>(xs: &Value, n: usize, h: &mut H, sp: Span) -> Result<Value> {
    if let Value::List(l) = xs {
        if let Some(v) = l.get(n) {
            return Ok(v.clone());
        }
    }
    let mut cur = xs.clone();
    for _ in 0..n {
        cur = cdr::<H>(&cur, h, sp)?;
    }
    car::<H>(&cur, h, sp)
}

fn last<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    if let Value::List(l) = &a[0] {
        if let Some(v) = l.last() {
            return Ok(v.clone());
        }
    }
    let mut cur = a[0].clone();
    loop {
        let tail = cdr::<H>(&cur, h, sp)?;
        if is_null(&tail) {
            return car::<H>(&cur, h, sp);
        }
        cur = tail;
    }
}

fn butlast<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    if let Value::List(l) = &a[0] {
        if !l.is_empty() {
            return Ok(if l.len() == 1 {
                Value::Nil
            } else {
                Value::List(Arc::new(l.take(l.len() - 1)))
            });
        }
    }
    let mut out = Vec::new();
    let mut cur = a[0].clone();
    loop {
        let tail = cdr::<H>(&cur, h, sp)?;
        if is_null(&tail) {
            return Ok(finish(out));
        }
        out.push(car::<H>(&cur, h, sp)?);
        cur = tail;
    }
}

fn safe_nth<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    if let (Value::Int(n), Value::List(l)) = (&a[0], &a[1]) {
        if l.is_empty() {
            return Ok(Value::Nil);
        }
        let i = usize::try_from((*n).max(0)).unwrap_or(usize::MAX);
        return Ok(l.get(i).cloned().unwrap_or(Value::Nil));
    }
    let mut n = a[0].clone();
    let mut cur = a[1].clone();
    loop {
        if is_null(&cur) {
            return Ok(Value::Nil);
        }
        if holds::<H>(prim_le::<H>, &n, Value::Int(0), h, sp)? {
            return car::<H>(&cur, h, sp);
        }
        n = prim_sub::<H>(&[n, Value::Int(1)], h, sp)?;
        cur = cdr::<H>(&cur, h, sp)?;
    }
}

fn range<H: 'static>(a: &[Value], h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    match a.len() {
        1 => range_impl::<H>(Value::Int(0), &a[0], &Value::Int(1), h, sp),
        2 => range_impl::<H>(a[0].clone(), &a[1], &Value::Int(1), h, sp),
        3 => range_impl::<H>(a[0].clone(), &a[1], &a[2], h, sp),
        _ => {
            c.apply_value(
                &native("display", Arity::Exact(1)),
                vec![Value::Str(Arc::from("range: arity 1, 2, or 3"))],
                h,
                sp,
            )?;
            c.apply_value(&native("newline", Arity::Exact(0)), Vec::new(), h, sp)
        }
    }
}

fn range_impl<H: 'static>(
    start: Value,
    end: &Value,
    step: &Value,
    h: &mut H,
    sp: Span,
) -> Result<Value> {
    if let (Value::Int(s), Value::Int(e), Value::Int(st)) = (&start, end, step) {
        let (mut cur, e, st) = (*s, *e, *st);
        if st == 0 {
            return Err(no_progress("range", sp));
        }
        let mut out = Vec::new();
        while !((st > 0 && cur >= e) || (st < 0 && cur <= e)) {
            out.push(Value::Int(cur));
            cur = cur
                .checked_add(st)
                .ok_or(EvalError::IntegerOverflow { op: "+", at: sp })?;
        }
        return Ok(finish(out));
    }
    let mut cur = start;
    let mut out = Vec::new();
    loop {
        let up = holds::<H>(prim_gt::<H>, step, Value::Int(0), h, sp)?
            && holds::<H>(prim_ge::<H>, &cur, end.clone(), h, sp)?;
        let down = !up
            && holds::<H>(prim_lt::<H>, step, Value::Int(0), h, sp)?
            && holds::<H>(prim_le::<H>, &cur, end.clone(), h, sp)?;
        if up || down {
            return Ok(finish(out));
        }
        let next = prim_add::<H>(&[cur.clone(), step.clone()], h, sp)?;
        if stalled(&cur, &next) {
            return Err(no_progress("range", sp));
        }
        out.push(cur);
        cur = next;
    }
}

fn repeat_list<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    if let Value::Int(n) = a[1] {
        let n = usize::try_from(n).unwrap_or(0);
        return Ok(finish(vec![a[0].clone(); n]));
    }
    let mut n = a[1].clone();
    let mut out = Vec::new();
    loop {
        if holds::<H>(prim_le::<H>, &n, Value::Int(0), h, sp)? {
            return Ok(finish(out));
        }
        let next = prim_sub::<H>(&[n.clone(), Value::Int(1)], h, sp)?;
        if stalled(&n, &next) {
            return Err(no_progress("repeat-list", sp));
        }
        out.push(a[0].clone());
        n = next;
    }
}

fn position_of(x: &Value, xs: &Value, sp: Span) -> Result<Option<usize>> {
    Ok(list_view(xs, sp)?.and_then(|l| l.iter().position(|e| value_eq_deep(e, x))))
}

fn zip_pairs(xs: &Value, ys: &Value, sp: Span) -> Result<Vec<Value>> {
    let xs = list_view(xs, sp)?;
    let ys = list_view(ys, sp)?;
    Ok(match (xs, ys) {
        (Some(xs), Some(ys)) => xs
            .iter()
            .zip(ys.iter())
            .map(|(a, b)| Value::list([a.clone(), b.clone()]))
            .collect(),
        _ => Vec::new(),
    })
}

fn intersperse<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    let sep = &a[0];
    if is_null(&a[1]) {
        return Ok(Value::Nil);
    }
    if let Value::List(l) = &a[1] {
        let mut out = Vec::with_capacity(l.len() * 2);
        for (i, x) in l.iter().enumerate() {
            if i > 0 {
                out.push(sep.clone());
            }
            out.push(x.clone());
        }
        return Ok(Value::list(out));
    }
    cdr::<H>(&a[1], h, sp)
}

fn flatten<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    let top = match &a[0] {
        v if is_null(v) => return Ok(Value::Nil),
        Value::List(l) => l.clone(),
        other => return car::<H>(other, h, sp),
    };
    let mut out = Vec::new();
    let mut stack: Vec<(Arc<List>, usize)> = vec![(top, 0)];
    while let Some((list, i)) = stack.pop() {
        let Some(x) = list.get(i) else { continue };
        let x = x.clone();
        stack.push((list, i + 1));
        match x {
            Value::Nil => {}
            Value::List(inner) => stack.push((inner, 0)),
            atom => out.push(atom),
        }
    }
    Ok(finish(out))
}

fn eq_hash(v: &Value, state: &mut DefaultHasher) {
    match v {
        Value::Nil => 0u8.hash(state),
        Value::Bool(b) => (1u8, b).hash(state),
        Value::Int(n) => (2u8, n).hash(state),
        Value::Float(f) => (3u8, if *f == 0.0 { 0 } else { f.to_bits() }).hash(state),
        Value::Str(s) => (4u8, &**s).hash(state),
        Value::Symbol(s) => (5u8, &**s).hash(state),
        Value::Keyword(s) => (6u8, &**s).hash(state),
        Value::List(l) => {
            (7u8, l.len()).hash(state);
            for x in l.iter() {
                eq_hash(x, state);
            }
        }
        Value::NativeFn(f) => (8u8, &*f.name).hash(state),
        Value::Closure(c) => (9u8, Arc::as_ptr(c) as usize).hash(state),
        Value::Sexp(..) => 10u8.hash(state),
        Value::Map(_) | Value::Promise(_) | Value::Error(_) | Value::Foreign(_) => {
            11u8.hash(state);
        }
    }
}

fn distinct(xs: &Value, sp: Span) -> Result<Value> {
    let Some(items) = list_view(xs, sp)? else {
        return Ok(Value::list([]));
    };
    let mut kept: Vec<Value> = Vec::new();
    let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
    for x in items {
        let mut state = DefaultHasher::new();
        eq_hash(x, &mut state);
        let bucket = buckets.entry(state.finish()).or_default();
        if !bucket.iter().any(|&i| value_eq_deep(&kept[i], x)) {
            bucket.push(kept.len());
            kept.push(x.clone());
        }
    }
    Ok(Value::list(kept))
}

fn best_by<H: 'static>(
    a: &[Value],
    better: fn(&[Value], &mut H, Span) -> Result<Value>,
    h: &mut H,
    c: &Caller<H>,
    sp: Span,
) -> Result<Value> {
    let f = &a[0];
    let mut best = car::<H>(&a[1], h, sp)?;
    let tail = cdr::<H>(&a[1], h, sp)?;
    if let Some(items) = list_view(&tail, sp)? {
        for x in items {
            let fx = c.call1(f, x.clone(), h, sp)?;
            let fb = c.call1(f, best.clone(), h, sp)?;
            if better(&[fx, fb], h, sp)?.is_truthy() {
                best = x.clone();
            }
        }
    }
    Ok(best)
}

fn parity<H: 'static>(x: &Value, want: i64, h: &mut H, sp: Span) -> Result<Value> {
    let r = prim_modulo::<H>(&[x.clone(), Value::Int(2)], h, sp)?;
    prim_num_eq::<H>(&[Value::Int(want), r], h, sp)
}

fn plist_get<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    let key = &a[1];
    if let Value::List(l) = &a[0] {
        let mut i = 0;
        while i + 1 < l.len() {
            if value_eq_deep(&l[i], key) {
                return Ok(l[i + 1].clone());
            }
            i += 2;
        }
        return Ok(Value::Nil);
    }
    if is_null(&a[0]) {
        return Ok(Value::Nil);
    }
    cdr::<H>(&a[0], h, sp)
}

fn force<H: 'static>(v: Value, h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    c.apply_value(&native("force", Arity::Exact(1)), vec![v], h, sp)
}

fn lazy_rest<H: 'static>(s: &Value, h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    let thunk = nth_by_cdr::<H>(s, 1, h, sp)?;
    force::<H>(thunk, h, c, sp)
}

fn lazy_take<H: 'static>(a: &[Value], h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    let mut left = a[0].clone();
    let mut stream = a[1].clone();
    let mut out = Vec::new();
    loop {
        if holds::<H>(prim_le::<H>, &left, Value::Int(0), h, sp)? || is_null(&stream) {
            return Ok(finish(out));
        }
        out.push(car::<H>(&stream, h, sp)?);
        left = prim_sub::<H>(&[left, Value::Int(1)], h, sp)?;
        stream = lazy_rest::<H>(&stream, h, c, sp)?;
    }
}

fn lazy_drop<H: 'static>(a: &[Value], h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    let mut left = a[0].clone();
    let mut stream = a[1].clone();
    loop {
        if holds::<H>(prim_le::<H>, &left, Value::Int(0), h, sp)? || is_null(&stream) {
            return Ok(stream);
        }
        left = prim_sub::<H>(&[left, Value::Int(1)], h, sp)?;
        stream = lazy_rest::<H>(&stream, h, c, sp)?;
    }
}

fn realize<H: 'static>(a: &[Value], h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    let mut s = a[0].clone();
    let mut out = Vec::new();
    while !is_null(&s) {
        out.push(car::<H>(&s, h, sp)?);
        s = lazy_rest::<H>(&s, h, c, sp)?;
    }
    Ok(finish(out))
}

fn get_in<H: 'static>(m: &Value, path: &Value, h: &mut H, sp: Span) -> Result<Value> {
    let mut m = m.clone();
    let mut path = path.clone();
    loop {
        if is_null(&path) {
            return Ok(m);
        }
        if is_null(&m) {
            return Ok(Value::Nil);
        }
        let k = car::<H>(&path, h, sp)?;
        m = map_get::<H>(&[m, k], h, sp)?;
        path = cdr::<H>(&path, h, sp)?;
    }
}

fn or_new_map(m: &Value) -> Value {
    if m.is_truthy() {
        m.clone()
    } else {
        Value::Map(Arc::new(Map::new()))
    }
}

fn assoc_in<H: 'static>(m: &Value, path: &Value, v: Value, h: &mut H, sp: Span) -> Result<Value> {
    if is_null(path) {
        return Ok(v);
    }
    let mut frames: Vec<(Value, Value)> = Vec::new();
    let mut m = m.clone();
    let mut path = path.clone();
    let mut inner = loop {
        let tail = cdr::<H>(&path, h, sp)?;
        let k = car::<H>(&path, h, sp)?;
        if is_null(&tail) {
            break map_set::<H>(vec![or_new_map(&m), k, v], h, sp)?;
        }
        let sub = or_new_map(&map_get::<H>(&[m.clone(), k.clone()], h, sp)?);
        frames.push((or_new_map(&m), k));
        m = sub;
        path = tail;
    };
    while let Some((target, k)) = frames.pop() {
        inner = map_set::<H>(vec![target, k, inner], h, sp)?;
    }
    Ok(inner)
}

fn frequencies<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    let mut out = Map::new();
    if let Some(items) = list_view(&a[0], sp)? {
        for x in items {
            let k = key_or_err(x, sp)?;
            let n = match out.get(&k) {
                Some(n) if !is_null(n) => prim_add::<H>(&[n.clone(), Value::Int(1)], h, sp)?,
                _ => Value::Int(1),
            };
            out.insert(k, n);
        }
    }
    Ok(Value::Map(Arc::new(out)))
}

fn group_by_into_map<H: 'static>(a: &[Value], h: &mut H, c: &Caller<H>, sp: Span) -> Result<Value> {
    let mut out = Map::new();
    if let Some(items) = list_view(&a[1], sp)? {
        for x in items {
            let key = key_or_err(&c.call1(&a[0], x.clone(), h, sp)?, sp)?;
            let current = match out.remove(&key) {
                Some(group) if group.is_truthy() => group,
                _ => Value::list([]),
            };
            let grown = prim_append::<H>(vec![current, Value::list([x.clone()])], h, sp)?;
            out.insert(key, grown);
        }
    }
    Ok(Value::Map(Arc::new(out)))
}

fn into_hash_map<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    let xs = &a[0];
    if is_null(xs) {
        return Ok(Value::Map(Arc::new(Map::new())));
    }
    let head = car::<H>(xs, h, sp)?;
    let pairs = matches!(&head, Value::List(l) if l.len() == 2);
    let Value::List(items) = xs else {
        return car::<H>(xs, h, sp);
    };
    if !pairs {
        return map_new::<H>(&items.to_vec(), h, sp);
    }
    let mut out = Map::new();
    for pair in items.iter() {
        let k = car::<H>(pair, h, sp)?;
        let v = nth_by_cdr::<H>(pair, 1, h, sp)?;
        out.insert(key_or_err(&k, sp)?, v);
    }
    Ok(Value::Map(Arc::new(out)))
}

fn select_keys<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    let mut out = Map::new();
    if let Some(keys) = list_view(&a[1], sp)? {
        for k in keys {
            if map_has::<H>(&[a[0].clone(), k.clone()], h, sp)?.is_truthy() {
                let v = map_get::<H>(&[a[0].clone(), k.clone()], h, sp)?;
                out.insert(key_or_err(k, sp)?, v);
            }
        }
    }
    Ok(Value::Map(Arc::new(out)))
}

fn count<H: 'static>(a: &[Value], h: &mut H, sp: Span) -> Result<Value> {
    Ok(match &a[0] {
        v if is_null(v) => Value::Int(0),
        Value::List(l) => int_of(l.len()),
        Value::Map(m) => int_of(m.len()),
        Value::Str(s) => int_of(s.chars().count()),
        _ => {
            return prim_error::<H>(
                &[
                    Value::Keyword(Arc::from("count")),
                    Value::Str(Arc::from("not countable")),
                ],
                h,
                sp,
            )
        }
    })
}
