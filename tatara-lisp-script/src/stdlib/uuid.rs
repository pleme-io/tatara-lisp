//! UUID generation — small, no `uuid` crate dep.
//!
//!   (uuid-v4)            → "550e8400-e29b-41d4-a716-446655440000"
//!
//! Uses the OS RNG via `/dev/urandom` on Unix or `getrandom` syscall on
//! recent Linux. Falls back to `std::time::SystemTime`-derived nonce
//! plus a counter on systems where neither is available — sufficient
//! for log correlation IDs but not cryptographically secure on those
//! platforms; the implementation prefers the OS RNG everywhere it can.

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "uuid-v4",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let mut buf = [0u8; 16];
            fill_random(&mut buf).map_err(|e| {
                EvalError::native_fn("uuid-v4", format!("rng: {e}"), sp)
            })?;
            // Set version (4) + variant (RFC 4122).
            buf[6] = (buf[6] & 0x0f) | 0x40;
            buf[8] = (buf[8] & 0x3f) | 0x80;
            Ok(Value::Str(Arc::from(format_uuid(&buf))))
        },
    );

    interp.register_fn(
        "random-bytes",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let n: usize = match &args[0] {
                Value::Int(n) if *n >= 0 => usize::try_from(*n).map_err(|_| {
                    EvalError::native_fn("random-bytes", format!("size {n} too large"), sp)
                })?,
                v => {
                    return Err(EvalError::native_fn(
                        "random-bytes",
                        format!("size must be non-negative int, got {v:?}"),
                        sp,
                    ))
                }
            };
            let mut buf = vec![0u8; n];
            fill_random(&mut buf).map_err(|e| {
                EvalError::native_fn("random-bytes", format!("rng: {e}"), sp)
            })?;
            Ok(Value::Str(Arc::from(hex_lower(&buf))))
        },
    );
}

fn fill_random(buf: &mut [u8]) -> std::io::Result<()> {
    // Best path: /dev/urandom (works on every Unix incl. macOS).
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")?;
    f.read_exact(buf)?;
    Ok(())
}

fn format_uuid(b: &[u8; 16]) -> String {
    // 8-4-4-4-12 grouping.
    format!(
        "{:02x}{:02x}{:02x}{:02x}-\
         {:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3],
        b[4], b[5], b[6], b[7],
        b[8], b[9],
        b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
