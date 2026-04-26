//! Structured logging to stderr. Level-aware — respects TATARA_LOG env
//! variable (debug | info | warn | error). Default level is info.
//!
//!   (log-debug MSG)
//!   (log-info  MSG)
//!   (log-warn  MSG)
//!   (log-error MSG)
//!
//! Each emits a timestamped, level-prefixed line. Scripts that want
//! structured JSON should use (json-stringify …) and (eprint-line).

use std::sync::Arc;

use tatara_lisp_eval::{Arity, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

const LEVEL_DEBUG: u8 = 0;
const LEVEL_INFO: u8 = 1;
const LEVEL_WARN: u8 = 2;
const LEVEL_ERROR: u8 = 3;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    for (name, lvl, label) in [
        ("log-debug", LEVEL_DEBUG, "DEBUG"),
        ("log-info", LEVEL_INFO, "INFO"),
        ("log-warn", LEVEL_WARN, "WARN"),
        ("log-error", LEVEL_ERROR, "ERROR"),
    ] {
        interp.register_fn(
            name,
            Arity::Exact(1),
            move |args: &[Value], _ctx: &mut ScriptCtx, sp| {
                let msg = str_arg(&args[0], name, sp)?;
                if lvl >= current_level() {
                    eprintln!("[{}] {} {}", now_hms(), label, msg);
                }
                Ok(Value::Nil)
            },
        );
    }
}

fn current_level() -> u8 {
    match std::env::var("TATARA_LOG")
        .ok()
        .as_deref()
        .unwrap_or("info")
    {
        "debug" => LEVEL_DEBUG,
        "warn" => LEVEL_WARN,
        "error" => LEVEL_ERROR,
        _ => LEVEL_INFO,
    }
}

fn now_hms() -> String {
    // HH:MM:SS (local-less; the timestamp is a relative breadcrumb, not a
    // precise wall-clock for audit). For audit, use (now-rfc3339) from time.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    Arc::<str>::from(format!("{h:02}:{m:02}:{s:02}")).to_string()
}
