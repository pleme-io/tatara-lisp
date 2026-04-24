//! Process / shell integration.
//!
//!   (exec-check CMD ARG…)        → 0 on success, non-zero exit code otherwise
//!                                   Streams stdin/stdout/stderr to the parent.
//!   (exec-capture CMD ARG…)      → ((:status N) (:stdout "…") (:stderr "…"))
//!                                   Captures stdout + stderr, exposes exit code.
//!   (exec-ok? CMD ARG…)          → bool; true iff exit code is 0
//!   (sh-exec STR)                → convenience: run STR through `sh -c`
//!                                   returning the capture-form result
//!
//! No implicit shell interpolation. Arguments are passed literally to
//! the underlying process; no glob / word-splitting / $VAR substitution.
//! Scripts that want shell features use `sh-exec` explicitly.

use std::process::{Command, Stdio};
use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "exec-check",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let (cmd, rest) = split_cmd(args, "exec-check", sp)?;
            let status = Command::new(&*cmd)
                .args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| EvalError::native_fn("exec-check", e.to_string(), sp))?;
            Ok(Value::Int(status.code().unwrap_or(-1) as i64))
        },
    );

    interp.register_fn(
        "exec-ok?",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let (cmd, rest) = split_cmd(args, "exec-ok?", sp)?;
            let status = Command::new(&*cmd)
                .args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| EvalError::native_fn("exec-ok?", e.to_string(), sp))?;
            Ok(Value::Bool(status.success()))
        },
    );

    interp.register_fn(
        "exec-capture",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let (cmd, rest) = split_cmd(args, "exec-capture", sp)?;
            let out = Command::new(&*cmd)
                .args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::null())
                .output()
                .map_err(|e| EvalError::native_fn("exec-capture", e.to_string(), sp))?;
            Ok(capture_result(&out))
        },
    );

    interp.register_fn(
        "sh-exec",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let script = str_arg(&args[0], "sh-exec", sp)?;
            let out = Command::new("sh")
                .arg("-c")
                .arg(&*script)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| EvalError::native_fn("sh-exec", e.to_string(), sp))?;
            Ok(capture_result(&out))
        },
    );
}

fn split_cmd(
    args: &[Value],
    fname: &'static str,
    sp: tatara_lisp::Span,
) -> Result<(Arc<str>, Vec<Arc<str>>), EvalError> {
    let mut it = args.iter();
    let cmd = str_arg(
        it.next().ok_or_else(|| {
            EvalError::native_fn(fname, "expected at least 1 argument".to_string(), sp)
        })?,
        fname,
        sp,
    )?;
    let rest = it
        .map(|v| str_arg(v, fname, sp))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((cmd, rest))
}

fn capture_result(out: &std::process::Output) -> Value {
    Value::list(vec![
        Value::list(vec![
            Value::Keyword(Arc::from("status")),
            Value::Int(out.status.code().unwrap_or(-1) as i64),
        ]),
        Value::list(vec![
            Value::Keyword(Arc::from("stdout")),
            Value::Str(Arc::from(String::from_utf8_lossy(&out.stdout).as_ref())),
        ]),
        Value::list(vec![
            Value::Keyword(Arc::from("stderr")),
            Value::Str(Arc::from(String::from_utf8_lossy(&out.stderr).as_ref())),
        ]),
    ])
}
