//! Environment-variable access.
//!
//!   (env-get NAME)                → string or nil
//!   (env-get NAME DEFAULT)        → string (default if missing)
//!   (env-required NAME)           → string or raises
//!   (argv)                        → list of strings (ScriptCtx::argv)
//!   (argv-get N)                  → nth arg, or nil if out of range
//!   (argv-get N DEFAULT)          → nth arg, or default

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "env-get",
        Arity::Range(1, 2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let name = str_arg(&args[0], "env-get", sp)?;
            match std::env::var(&*name) {
                Ok(v) => Ok(Value::Str(Arc::from(v))),
                Err(_) => Ok(args.get(1).cloned().unwrap_or(Value::Nil)),
            }
        },
    );

    interp.register_fn(
        "env-required",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let name = str_arg(&args[0], "env-required", sp)?;
            std::env::var(&*name).map(|v| Value::Str(Arc::from(v))).map_err(|_| {
                EvalError::native_fn(
                    "env-required",
                    format!("environment variable {name} is not set"),
                    sp,
                )
            })
        },
    );

    interp.register_fn(
        "argv",
        Arity::Exact(0),
        |_args: &[Value], ctx: &mut ScriptCtx, _sp| {
            Ok(Value::list(
                ctx.argv
                    .iter()
                    .map(|s| Value::Str(Arc::from(s.as_str())))
                    .collect::<Vec<_>>(),
            ))
        },
    );

    interp.register_fn(
        "argv-get",
        Arity::Range(1, 2),
        |args: &[Value], ctx: &mut ScriptCtx, sp| {
            let n = int_arg(&args[0], "argv-get", sp)?;
            if n < 0 {
                return Err(EvalError::native_fn(
                    "argv-get",
                    format!("index must be >= 0, got {n}"),
                    sp,
                ));
            }
            let idx = n as usize;
            if idx < ctx.argv.len() {
                Ok(Value::Str(Arc::from(ctx.argv[idx].as_str())))
            } else {
                Ok(args.get(1).cloned().unwrap_or(Value::Nil))
            }
        },
    );
}

pub(crate) fn str_arg(
    v: &Value,
    fname: &'static str,
    sp: tatara_lisp::Span,
) -> Result<Arc<str>, EvalError> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        other => Err(EvalError::type_mismatch("string", other.type_name(), sp)).map_err(|e| {
            EvalError::native_fn(fname, format!("expected string, got {e:?}"), sp)
        }),
    }
}

pub(crate) fn int_arg(
    v: &Value,
    fname: &'static str,
    sp: tatara_lisp::Span,
) -> Result<i64, EvalError> {
    match v {
        Value::Int(n) => Ok(*n),
        other => Err(EvalError::native_fn(
            fname,
            format!("expected integer, got {}", other.type_name()),
            sp,
        )),
    }
}
