//! OS introspection — the few knobs scripts actually need.
//!
//!   (os-platform)    → "linux" | "macos" | "windows" | "other"
//!   (os-arch)        → "x86_64" | "aarch64" | ...
//!   (hostname)       → machine hostname string
//!   (username)       → $USER / $USERNAME / system user
//!   (user-home)      → $HOME expanded

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "os-platform",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            let p = if cfg!(target_os = "linux") {
                "linux"
            } else if cfg!(target_os = "macos") {
                "macos"
            } else if cfg!(target_os = "windows") {
                "windows"
            } else {
                "other"
            };
            Ok(Value::Str(Arc::from(p)))
        },
    );

    interp.register_fn(
        "os-arch",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            Ok(Value::Str(Arc::from(std::env::consts::ARCH)))
        },
    );

    interp.register_fn(
        "hostname",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            // Read /etc/hostname on unix-like; fall back to HOSTNAME env.
            if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
                return Ok(Value::Str(Arc::from(h.trim())));
            }
            if let Ok(h) = std::env::var("HOSTNAME") {
                return Ok(Value::Str(Arc::from(h.as_str())));
            }
            Err(EvalError::native_fn(
                "hostname",
                "could not determine hostname",
                sp,
            ))
        },
    );

    interp.register_fn(
        "username",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            for k in ["USER", "USERNAME", "LOGNAME"] {
                if let Ok(u) = std::env::var(k) {
                    return Ok(Value::Str(Arc::from(u.as_str())));
                }
            }
            Err(EvalError::native_fn(
                "username",
                "USER/USERNAME/LOGNAME all unset",
                sp,
            ))
        },
    );

    interp.register_fn(
        "user-home",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            std::env::var("HOME")
                .map(|h| Value::Str(Arc::from(h.as_str())))
                .map_err(|_| EvalError::native_fn("user-home", "HOME not set", sp))
        },
    );
}
