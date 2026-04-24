//! File I/O + stdout/stderr.
//!
//!   (print-line STR)              → STR to stdout + newline, returns nil
//!   (eprint-line STR)             → STR to stderr + newline, returns nil
//!   (read-file PATH)              → file contents as string
//!   (write-file PATH STR)         → nil (truncates if exists)
//!   (path-exists? PATH)           → bool
//!   (exit CODE)                   → never returns; terminates process

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::{int_arg, str_arg};

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "print-line",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "print-line", sp)?;
            println!("{s}");
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "eprint-line",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "eprint-line", sp)?;
            eprintln!("{s}");
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "read-file",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "read-file", sp)?;
            let contents = std::fs::read_to_string(&*path)
                .map_err(|e| EvalError::native_fn("read-file", format!("{path}: {e}"), sp))?;
            Ok(Value::Str(Arc::from(contents)))
        },
    );

    interp.register_fn(
        "write-file",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "write-file", sp)?;
            let body = str_arg(&args[1], "write-file", sp)?;
            std::fs::write(&*path, body.as_bytes())
                .map_err(|e| EvalError::native_fn("write-file", format!("{path}: {e}"), sp))?;
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "path-exists?",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "path-exists?", sp)?;
            Ok(Value::Bool(std::path::Path::new(&*path).exists()))
        },
    );

    interp.register_fn(
        "exit",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let code = int_arg(&args[0], "exit", sp)?;
            std::process::exit(code as i32)
        },
    );
}
