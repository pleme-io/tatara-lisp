//! Extended list operators beyond tatara-lisp-eval's primitives.
//!
//!   (take N XS)       → first N elements
//!   (drop N XS)       → all elements after the first N
//!   (range LO HI)     → integers [LO, HI)
//!   (range LO HI STEP) → integers [LO, HI) stepping by STEP
//!   (distinct XS)     → same order, duplicates removed
//!   (flatten XS)      → one-level flatten
//!   (zip XS YS)       → list of 2-lists
//!   (concat XS YS …)  → append any number of lists
//!   (nth N XS)        → 0-based; nil if out of range
//!   (last XS)         → last element or nil

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::int_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "take",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let n = int_arg(&args[0], "take", sp)?.max(0) as usize;
            let xs = list_arg(&args[1], "take", sp)?;
            Ok(Value::list(xs.iter().take(n).cloned().collect::<Vec<_>>()))
        },
    );

    interp.register_fn(
        "drop",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let n = int_arg(&args[0], "drop", sp)?.max(0) as usize;
            let xs = list_arg(&args[1], "drop", sp)?;
            Ok(Value::list(xs.iter().skip(n).cloned().collect::<Vec<_>>()))
        },
    );

    interp.register_fn(
        "range",
        Arity::Range(2, 3),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let lo = int_arg(&args[0], "range", sp)?;
            let hi = int_arg(&args[1], "range", sp)?;
            let step = args
                .get(2)
                .map(|v| int_arg(v, "range", sp))
                .transpose()?
                .unwrap_or(1);
            if step == 0 {
                return Err(EvalError::native_fn("range", "step cannot be 0", sp));
            }
            let mut out = Vec::new();
            let mut i = lo;
            while (step > 0 && i < hi) || (step < 0 && i > hi) {
                out.push(Value::Int(i));
                i += step;
            }
            Ok(Value::list(out))
        },
    );

    interp.register_fn(
        "distinct",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let xs = list_arg(&args[0], "distinct", sp)?;
            // Value doesn't impl PartialEq directly (it holds Arc<dyn Any>
            // for Foreign); compare via Debug-formatted string. Cheap +
            // good-enough for script-level "remove duplicates by structure".
            let mut seen_keys: Vec<String> = Vec::new();
            let mut out: Vec<Value> = Vec::new();
            for item in xs.iter() {
                let key = format!("{item:?}");
                if !seen_keys.contains(&key) {
                    seen_keys.push(key);
                    out.push(item.clone());
                }
            }
            Ok(Value::list(out))
        },
    );

    interp.register_fn(
        "flatten",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let xs = list_arg(&args[0], "flatten", sp)?;
            let mut out = Vec::new();
            for item in xs.iter() {
                match item {
                    Value::List(inner) => out.extend(inner.iter().cloned()),
                    Value::Nil => {}
                    other => out.push(other.clone()),
                }
            }
            Ok(Value::list(out))
        },
    );

    interp.register_fn(
        "zip",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let xs = list_arg(&args[0], "zip", sp)?;
            let ys = list_arg(&args[1], "zip", sp)?;
            let out: Vec<Value> = xs
                .iter()
                .zip(ys.iter())
                .map(|(a, b)| Value::list(vec![a.clone(), b.clone()]))
                .collect();
            Ok(Value::list(out))
        },
    );

    interp.register_fn(
        "concat",
        Arity::AtLeast(0),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let mut out = Vec::new();
            for v in args {
                let xs = list_arg(v, "concat", sp)?;
                out.extend(xs.iter().cloned());
            }
            Ok(Value::list(out))
        },
    );

    interp.register_fn(
        "nth",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let n = int_arg(&args[0], "nth", sp)?;
            let xs = list_arg(&args[1], "nth", sp)?;
            if n < 0 {
                return Ok(Value::Nil);
            }
            Ok(xs.get(n as usize).cloned().unwrap_or(Value::Nil))
        },
    );

    interp.register_fn(
        "last",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let xs = list_arg(&args[0], "last", sp)?;
            Ok(xs.last().cloned().unwrap_or(Value::Nil))
        },
    );
}

fn list_arg(
    v: &Value,
    fname: &'static str,
    sp: tatara_lisp::Span,
) -> Result<Arc<Vec<Value>>, EvalError> {
    match v {
        Value::List(xs) => Ok(xs.clone()),
        Value::Nil => Ok(Arc::new(Vec::new())),
        other => Err(EvalError::native_fn(
            fname,
            format!("expected list, got {}", other.type_name()),
            sp,
        )),
    }
}
