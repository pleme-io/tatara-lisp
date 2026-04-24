//! SOPS-decrypt primitive. Shells out to the system `sops` binary — no
//! point re-implementing age/PGP in-process when the CLI is a one-liner
//! and all the surrounding tooling already assumes it.
//!
//!   (sops-extract PATH EXPR) → string
//!
//! EXPR uses sops' JSON-path syntax: `["cloudflare"]["api-token"]`.

use std::process::Command;
use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "sops-extract",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "sops-extract", sp)?;
            let expr = str_arg(&args[1], "sops-extract", sp)?;

            // Prefer a sops binary on PATH; error loudly if absent.
            let sops = which::which("sops").map_err(|e| {
                EvalError::native_fn(
                    "sops-extract",
                    format!("sops not found on PATH: {e}"),
                    sp,
                )
            })?;

            let out = Command::new(&sops)
                .args(["-d", "--extract", &expr, &path])
                .output()
                .map_err(|e| EvalError::native_fn("sops-extract", e.to_string(), sp))?;

            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Err(EvalError::native_fn(
                    "sops-extract",
                    format!("sops failed ({}): {stderr}", out.status),
                    sp,
                ));
            }

            let body = String::from_utf8(out.stdout).map_err(|e| {
                EvalError::native_fn("sops-extract", format!("non-utf8 output: {e}"), sp)
            })?;
            // sops -d --extract sometimes includes a trailing newline; strip it.
            Ok(Value::Str(Arc::from(body.trim_end_matches('\n'))))
        },
    );
}
