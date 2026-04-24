//! Regular expressions (rust `regex` crate under the hood).
//!
//!   (re-match? PATTERN STR)      → bool
//!   (re-find PATTERN STR)        → first match as string, or nil
//!   (re-find-all PATTERN STR)    → list of match strings
//!   (re-captures PATTERN STR)    → list of capture groups for first match
//!                                  (returns nil if no match)
//!   (re-replace PATTERN STR REPL) → every match replaced; $1/$2… refer
//!                                    to capture groups in REPL
//!   (re-split PATTERN STR)       → STR split on every PATTERN match

use std::sync::Arc;

use regex::Regex;
use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "re-match?",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let re = compile(&args[0], sp)?;
            let s = str_arg(&args[1], "re-match?", sp)?;
            Ok(Value::Bool(re.is_match(&s)))
        },
    );

    interp.register_fn(
        "re-find",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let re = compile(&args[0], sp)?;
            let s = str_arg(&args[1], "re-find", sp)?;
            Ok(re
                .find(&s)
                .map(|m| Value::Str(Arc::from(m.as_str())))
                .unwrap_or(Value::Nil))
        },
    );

    interp.register_fn(
        "re-find-all",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let re = compile(&args[0], sp)?;
            let s = str_arg(&args[1], "re-find-all", sp)?;
            Ok(Value::list(
                re.find_iter(&s)
                    .map(|m| Value::Str(Arc::from(m.as_str())))
                    .collect::<Vec<_>>(),
            ))
        },
    );

    interp.register_fn(
        "re-captures",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let re = compile(&args[0], sp)?;
            let s = str_arg(&args[1], "re-captures", sp)?;
            let Some(caps) = re.captures(&s) else {
                return Ok(Value::Nil);
            };
            let groups: Vec<Value> = caps
                .iter()
                .map(|m| m.map_or(Value::Nil, |m| Value::Str(Arc::from(m.as_str()))))
                .collect();
            Ok(Value::list(groups))
        },
    );

    interp.register_fn(
        "re-replace",
        Arity::Exact(3),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let re = compile(&args[0], sp)?;
            let s = str_arg(&args[1], "re-replace", sp)?;
            let repl = str_arg(&args[2], "re-replace", sp)?;
            Ok(Value::Str(Arc::from(re.replace_all(&s, &*repl).into_owned())))
        },
    );

    interp.register_fn(
        "re-split",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let re = compile(&args[0], sp)?;
            let s = str_arg(&args[1], "re-split", sp)?;
            Ok(Value::list(
                re.split(&s)
                    .map(|p| Value::Str(Arc::from(p)))
                    .collect::<Vec<_>>(),
            ))
        },
    );
}

fn compile(v: &Value, sp: tatara_lisp::Span) -> Result<Regex, EvalError> {
    let pat = str_arg(v, "regex", sp)?;
    Regex::new(&pat).map_err(|e| {
        EvalError::native_fn("regex", format!("bad pattern {pat:?}: {e}"), sp)
    })
}
