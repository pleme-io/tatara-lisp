//! Extended string operators beyond tatara-lisp-eval primitives.
//!
//!   (string-starts-with? STR PREFIX) → bool
//!   (string-ends-with?   STR SUFFIX) → bool
//!   (string-repeat STR N)            → STR repeated N times
//!   (string-reverse STR)             → reversed (by chars, not bytes)
//!   (string-join SEP XS)             → concatenate XS with SEP between
//!   (string-chars STR)               → list of 1-char strings
//!   (string-bytes STR)               → list of integers 0..=255
//!   (string-uppercase STR)           → uppercase (rounds out -lowercase)

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::{int_arg, str_arg};

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "string-starts-with?",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-starts-with?", sp)?;
            let pfx = str_arg(&args[1], "string-starts-with?", sp)?;
            Ok(Value::Bool(s.starts_with(&*pfx)))
        },
    );

    interp.register_fn(
        "string-ends-with?",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-ends-with?", sp)?;
            let sfx = str_arg(&args[1], "string-ends-with?", sp)?;
            Ok(Value::Bool(s.ends_with(&*sfx)))
        },
    );

    interp.register_fn(
        "string-repeat",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-repeat", sp)?;
            let n = int_arg(&args[1], "string-repeat", sp)?.max(0) as usize;
            Ok(Value::Str(Arc::from(s.repeat(n))))
        },
    );

    interp.register_fn(
        "string-reverse",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-reverse", sp)?;
            Ok(Value::Str(Arc::from(
                s.chars().rev().collect::<String>(),
            )))
        },
    );

    interp.register_fn(
        "string-join",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let sep = str_arg(&args[0], "string-join", sp)?;
            let xs = match &args[1] {
                Value::List(xs) => xs.clone(),
                Value::Nil => Arc::new(Vec::new()),
                other => {
                    return Err(EvalError::native_fn(
                        "string-join",
                        format!("expected list, got {}", other.type_name()),
                        sp,
                    ))
                }
            };
            let parts: Vec<String> = xs
                .iter()
                .map(|v| match v {
                    Value::Str(s) => s.as_ref().to_owned(),
                    Value::Int(n) => n.to_string(),
                    Value::Float(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Nil => String::new(),
                    Value::Symbol(s) | Value::Keyword(s) => s.as_ref().to_owned(),
                    other => format!("{other:?}"),
                })
                .collect();
            Ok(Value::Str(Arc::from(parts.join(&sep))))
        },
    );

    interp.register_fn(
        "string-chars",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-chars", sp)?;
            Ok(Value::list(
                s.chars()
                    .map(|c| Value::Str(Arc::from(c.to_string())))
                    .collect::<Vec<_>>(),
            ))
        },
    );

    interp.register_fn(
        "string-bytes",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-bytes", sp)?;
            Ok(Value::list(
                s.bytes().map(|b| Value::Int(b as i64)).collect::<Vec<_>>(),
            ))
        },
    );

    interp.register_fn(
        "string-uppercase",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-uppercase", sp)?;
            Ok(Value::Str(Arc::from(s.to_uppercase())))
        },
    );
}
