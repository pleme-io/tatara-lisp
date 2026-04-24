//! String helpers beyond what tatara-lisp-eval::primitive provides.
//!
//!   (string-replace STR FROM TO)  → new string
//!   (string-split STR SEP)        → list of strings
//!   (string-contains? STR NEEDLE) → bool
//!   (string-lowercase STR)        → new string
//!   (string-format TMPL &rest VS) → printf-ish, `{}` placeholders consumed left-to-right
//!   (string-trim STR)             → new string with leading/trailing whitespace removed

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "string-replace",
        Arity::Exact(3),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-replace", sp)?;
            let from = str_arg(&args[1], "string-replace", sp)?;
            let to = str_arg(&args[2], "string-replace", sp)?;
            Ok(Value::Str(Arc::from(s.replace(&*from, &to))))
        },
    );

    interp.register_fn(
        "string-split",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-split", sp)?;
            let sep = str_arg(&args[1], "string-split", sp)?;
            Ok(Value::list(
                s.split(&*sep)
                    .map(|p| Value::Str(Arc::from(p)))
                    .collect(),
            ))
        },
    );

    interp.register_fn(
        "string-contains?",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-contains?", sp)?;
            let needle = str_arg(&args[1], "string-contains?", sp)?;
            Ok(Value::Bool(s.contains(&*needle)))
        },
    );

    interp.register_fn(
        "string-lowercase",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-lowercase", sp)?;
            Ok(Value::Str(Arc::from(s.to_lowercase())))
        },
    );

    interp.register_fn(
        "string-trim",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "string-trim", sp)?;
            Ok(Value::Str(Arc::from(s.trim())))
        },
    );

    interp.register_fn(
        "string-format",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let tmpl = str_arg(&args[0], "string-format", sp)?;
            let mut out = String::with_capacity(tmpl.len());
            let mut remaining = &*tmpl;
            let mut idx = 1usize;
            while let Some(pos) = remaining.find("{}") {
                out.push_str(&remaining[..pos]);
                let Some(arg) = args.get(idx) else {
                    return Err(EvalError::native_fn(
                        "string-format",
                        format!("template has more {{}} than args ({idx})"),
                        sp,
                    ));
                };
                out.push_str(&display_value(arg));
                remaining = &remaining[pos + 2..];
                idx += 1;
            }
            out.push_str(remaining);
            Ok(Value::Str(Arc::from(out)))
        },
    );
}

fn display_value(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Str(s) | Value::Symbol(s) | Value::Keyword(s) => s.as_ref().to_string(),
        other => format!("{other:?}"),
    }
}
