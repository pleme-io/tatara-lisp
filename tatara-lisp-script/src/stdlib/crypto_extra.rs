//! Additional hashes + HMAC + random/uuid.
//!
//!   (sha1 STR)                → hex digest
//!   (sha512 STR)              → hex digest
//!   (hmac-sha256 KEY MESSAGE) → hex digest
//!   (uuid-v4)                 → RFC 4122 v4 UUID string
//!   (random-hex N)            → N bytes of crypto-random hex (2*N chars)
//!   (random-int LO HI)        → uniform random in [LO, HI)

use std::sync::Arc;

use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::{int_arg, str_arg};

type HmacSha256 = Hmac<Sha256>;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "sha1",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "sha1", sp)?;
            Ok(Value::Str(Arc::from(hex::encode(Sha1::digest(
                s.as_bytes(),
            )))))
        },
    );

    interp.register_fn(
        "sha512",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "sha512", sp)?;
            Ok(Value::Str(Arc::from(hex::encode(Sha512::digest(
                s.as_bytes(),
            )))))
        },
    );

    interp.register_fn(
        "hmac-sha256",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let key = str_arg(&args[0], "hmac-sha256", sp)?;
            let msg = str_arg(&args[1], "hmac-sha256", sp)?;
            let mut mac = HmacSha256::new_from_slice(key.as_bytes())
                .map_err(|e| EvalError::native_fn("hmac-sha256", e.to_string(), sp))?;
            mac.update(msg.as_bytes());
            Ok(Value::Str(Arc::from(hex::encode(
                mac.finalize().into_bytes(),
            ))))
        },
    );

    interp.register_fn(
        "uuid-v4",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| Ok(Value::Str(Arc::from(uuid_v4()))),
    );

    interp.register_fn(
        "random-hex",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let n = int_arg(&args[0], "random-hex", sp)?;
            if n < 0 {
                return Err(EvalError::native_fn(
                    "random-hex",
                    format!("need >= 0 bytes, got {n}"),
                    sp,
                ));
            }
            let bytes = random_bytes(n as usize);
            Ok(Value::Str(Arc::from(hex::encode(bytes))))
        },
    );

    interp.register_fn(
        "random-int",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let lo = int_arg(&args[0], "random-int", sp)?;
            let hi = int_arg(&args[1], "random-int", sp)?;
            if hi <= lo {
                return Err(EvalError::native_fn(
                    "random-int",
                    format!("hi ({hi}) must be > lo ({lo})"),
                    sp,
                ));
            }
            let range = (hi - lo) as u64;
            let r = {
                let bytes = random_bytes(8);
                let mut u = 0u64;
                for b in bytes {
                    u = (u << 8) | b as u64;
                }
                lo + (u % range) as i64
            };
            Ok(Value::Int(r))
        },
    );
}

fn random_bytes(n: usize) -> Vec<u8> {
    // Mix /dev/urandom (when available) with a process-unique fallback.
    let mut out = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut out).is_ok() {
            return out;
        }
    }
    // Fallback: weak (timestamp + pid + counter) — scripts should not rely
    // on this, but it's better than zeros if /dev/urandom is absent.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    for (i, b) in out.iter_mut().enumerate() {
        let seed = ts
            .wrapping_add(pid)
            .wrapping_add(i as u64)
            .wrapping_mul(2_862_933_555_777_941_757);
        *b = (seed >> 32) as u8;
    }
    out
}

fn uuid_v4() -> String {
    let mut bytes = random_bytes(16);
    // v4 variant bits
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}
