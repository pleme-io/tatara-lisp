//! Common text encodings.
//!
//!   (base64-encode STR)     → standard base64 (with padding)
//!   (base64-decode STR)     → decoded string (non-utf8 = error)
//!   (base64url-encode STR)  → URL-safe base64, no padding
//!   (base64url-decode STR)  → decoded
//!   (url-encode STR)        → percent-encoded (RFC 3986 unreserved)
//!   (url-decode STR)        → percent-decoded
//!   (hex-encode STR)        → lowercase hex (bytes of STR)
//!   (hex-decode STR)        → string (non-utf8 = error)

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    use base64::Engine;

    interp.register_fn(
        "base64-encode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let engine = base64::engine::general_purpose::STANDARD;
            let s = str_arg(&args[0], "base64-encode", sp)?;
            Ok(Value::Str(Arc::from(engine.encode(s.as_bytes()))))
        },
    );

    interp.register_fn(
        "base64-decode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let engine = base64::engine::general_purpose::STANDARD;
            let s = str_arg(&args[0], "base64-decode", sp)?;
            let bytes = engine
                .decode(s.as_bytes())
                .map_err(|e| EvalError::native_fn("base64-decode", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(String::from_utf8(bytes).map_err(
                |e| EvalError::native_fn("base64-decode", format!("not utf8: {e}"), sp),
            )?)))
        },
    );

    interp.register_fn(
        "base64url-encode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            let s = str_arg(&args[0], "base64url-encode", sp)?;
            Ok(Value::Str(Arc::from(engine.encode(s.as_bytes()))))
        },
    );

    interp.register_fn(
        "base64url-decode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            let s = str_arg(&args[0], "base64url-decode", sp)?;
            let bytes = engine
                .decode(s.as_bytes())
                .map_err(|e| EvalError::native_fn("base64url-decode", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(String::from_utf8(bytes).map_err(
                |e| EvalError::native_fn("base64url-decode", format!("not utf8: {e}"), sp),
            )?)))
        },
    );

    interp.register_fn(
        "url-encode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "url-encode", sp)?;
            Ok(Value::Str(Arc::from(url_pct_encode(&s))))
        },
    );

    interp.register_fn(
        "url-decode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "url-decode", sp)?;
            Ok(Value::Str(Arc::from(
                url_pct_decode(&s).map_err(|e| EvalError::native_fn("url-decode", e, sp))?,
            )))
        },
    );

    interp.register_fn(
        "hex-encode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "hex-encode", sp)?;
            Ok(Value::Str(Arc::from(hex::encode(s.as_bytes()))))
        },
    );

    interp.register_fn(
        "hex-decode",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "hex-decode", sp)?;
            let bytes = hex::decode(s.as_bytes())
                .map_err(|e| EvalError::native_fn("hex-decode", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(String::from_utf8(bytes).map_err(
                |e| EvalError::native_fn("hex-decode", format!("not utf8: {e}"), sp),
            )?)))
        },
    );
}

fn url_pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn url_pct_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(format!("truncated percent-escape at {i}"));
            }
            let hex_str = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|e| format!("bad utf8 in escape: {e}"))?;
            let byte =
                u8::from_str_radix(hex_str, 16).map_err(|e| format!("bad hex {hex_str:?}: {e}"))?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|e| format!("non-utf8 result: {e}"))
}
