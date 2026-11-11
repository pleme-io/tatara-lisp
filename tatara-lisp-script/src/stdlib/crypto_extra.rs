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
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            uuid_v4()
                .map(|id| Value::Str(Arc::from(id)))
                .map_err(|e| EvalError::native_fn("uuid-v4", e.to_string(), sp))
        },
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
            let bytes = random_bytes(n as usize)
                .map_err(|e| EvalError::native_fn("random-hex", e.to_string(), sp))?;
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
            let range = hi.abs_diff(lo);
            let draw = || {
                random_bytes(8)
                    .map(|bytes| u64::from_be_bytes(bytes.try_into().expect("8 bytes")))
                    .map_err(|e| EvalError::native_fn("random-int", e.to_string(), sp))
            };
            let offset = uniform_below(range, draw)?;
            Ok(Value::Int(lo.wrapping_add_unsigned(offset)))
        },
    );
}

const ENTROPY_SOURCE: &str = "/dev/urandom";

fn random_bytes(n: usize) -> std::io::Result<Vec<u8>> {
    random_bytes_from(std::path::Path::new(ENTROPY_SOURCE), n)
}

fn random_bytes_from(source: &std::path::Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut out = vec![0u8; n];
    std::fs::File::open(source)?.read_exact(&mut out)?;
    Ok(out)
}

fn uniform_below<E>(range: u64, mut draw: impl FnMut() -> Result<u64, E>) -> Result<u64, E> {
    let rejected_below = range.wrapping_neg() % range;
    loop {
        let value = draw()?;
        if value >= rejected_below {
            return Ok(value % range);
        }
    }
}

fn uuid_v4() -> std::io::Result<String> {
    let mut bytes = random_bytes(16)?;
    // v4 variant bits
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_entropy_source_is_an_error_not_a_clock_fallback() {
        let missing = std::path::Path::new("/nonexistent/tatara-entropy");
        assert!(random_bytes_from(missing, 16).is_err());
    }

    #[test]
    fn uniform_below_rejects_the_biased_tail() {
        let mut draws = [0u64, 1, 2, 3, u64::MAX].into_iter();
        let range = 3 * (1u64 << 62);
        let first = uniform_below(range, || Ok::<_, ()>(draws.next().unwrap())).unwrap();
        assert_eq!(first, u64::MAX % range);
    }

    #[test]
    fn random_bytes_reads_the_os_source() {
        let a = random_bytes(32).unwrap();
        let b = random_bytes(32).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
