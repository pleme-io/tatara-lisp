//! Hash functions.
//!
//!   (sha256 STR)         → hex digest string
//!   (sha256-file PATH)   → hex digest of the file's BYTES
//!   (blake3-file PATH)   → hex digest of the file's BYTES
//!   (slugify NAME TYPE) → slug matching Pangea::Architectures::
//!                         CloudflareDnsRecords.derive_slug. Useful when a
//!                         script is emitting tofu import commands whose
//!                         resource addresses come from the Ruby
//!                         architecture's naming convention.

//! `sha256` takes a STRING and is therefore unusable on a release artifact:
//! the only way to get file contents into the interpreter is `read-file`,
//! which is `std::fs::read_to_string` and rejects any non-UTF-8 byte, so an
//! ELF binary is a hard error before it reaches a hasher. `sha256-file` and
//! `blake3-file` take a PATH and stream the bytes, so nothing ever has to be
//! representable as a tlisp string. That is also why they return only a hex
//! digest and never the bytes — the six closed atom kinds have no byte-string
//! arm, and adding one to hash a file would be the wrong end of the problem.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "sha256",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "sha256", sp)?;
            let digest = Sha256::digest(s.as_bytes());
            Ok(Value::Str(Arc::from(hex::encode(digest))))
        },
    );

    interp.register_fn(
        "sha256-file",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "sha256-file", sp)?;
            let mut hasher = Sha256::new();
            stream_into(&path, &mut hasher)
                .map_err(|e| EvalError::native_fn("sha256-file", e, sp))?;
            Ok(Value::Str(Arc::from(hex::encode(hasher.finalize()))))
        },
    );

    interp.register_fn(
        "blake3-file",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "blake3-file", sp)?;
            let mut hasher = blake3::Hasher::new();
            stream_into(&path, &mut hasher)
                .map_err(|e| EvalError::native_fn("blake3-file", e, sp))?;
            Ok(Value::Str(Arc::from(hasher.finalize().to_hex().to_string())))
        },
    );

    interp.register_fn(
        "slugify",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let name = str_arg(&args[0], "slugify", sp)?;
            let kind = str_arg(&args[1], "slugify", sp)?;
            Ok(Value::Str(Arc::from(derive_slug(&name, &kind))))
        },
    );
}

/// Stream a file's bytes into a hasher.
///
/// Streamed rather than read whole so hashing a multi-hundred-megabyte
/// release artifact costs a fixed buffer instead of its size in RAM, and so
/// that the bytes never exist as a `String` — which is what makes this work
/// on a binary at all.
fn stream_into<W: std::io::Write>(path: &str, hasher: &mut W) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    std::io::copy(&mut file, hasher).map_err(|e| format!("{path}: {e}"))?;
    Ok(())
}

/// Mirror of Pangea::Architectures::CloudflareDnsRecords.derive_slug so
/// tlisp scripts emit the same Terraform resource addresses as the Ruby
/// architecture. Any drift here is a bug — keep this and the Ruby method
/// in lockstep.
pub fn derive_slug(name: &str, kind: &str) -> String {
    let normalized = if name == "@" || name.is_empty() {
        "root".to_string()
    } else {
        let mut s = name.replace('.', "_");
        // "*_foo" → "wildcard_foo"
        if let Some(stripped) = s.strip_prefix("*_") {
            s = format!("wildcard_{stripped}");
        } else if let Some(stripped) = s.strip_prefix("*") {
            s = format!("wildcard{stripped}");
        }
        // Collapse runs of underscores so "resend__domainkey" → "resend_domainkey".
        while s.contains("__") {
            s = s.replace("__", "_");
        }
        s
    };
    format!("{}_{}", normalized, kind.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apex_normalizes_to_root() {
        assert_eq!(derive_slug("@", "CNAME"), "root_cname");
    }

    #[test]
    fn dots_become_underscores() {
        assert_eq!(derive_slug("api.staging", "CNAME"), "api_staging_cname");
    }

    #[test]
    fn wildcard_expands() {
        assert_eq!(derive_slug("*.staging", "CNAME"), "wildcard_staging_cname");
    }

    #[test]
    fn underscore_prefixed_dkim_preserved() {
        assert_eq!(
            derive_slug("resend._domainkey", "TXT"),
            "resend_domainkey_txt"
        );
    }

    #[test]
    fn plain_name_lowercases_type() {
        assert_eq!(derive_slug("www", "CNAME"), "www_cname");
        assert_eq!(derive_slug("send", "MX"), "send_mx");
    }
}

#[cfg(test)]
mod file_hash_tests {
    use super::*;
    use std::io::Write;

    /// The empty-input digests are the published constants for both
    /// algorithms, so a wiring mistake that hashed a filename, a path, or a
    /// buffer of the wrong length cannot pass.
    #[test]
    fn empty_file_matches_published_constants() {
        let dir = std::env::temp_dir().join("tl-hash-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty");
        std::fs::write(&p, b"").unwrap();
        let path = p.to_str().unwrap();

        let mut s = Sha256::new();
        stream_into(path, &mut s).unwrap();
        assert_eq!(
            hex::encode(s.finalize()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let mut b = blake3::Hasher::new();
        stream_into(path, &mut b).unwrap();
        assert_eq!(
            b.finalize().to_hex().to_string(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    /// THE POINT OF THESE PRIMITIVES. 0x80 is not valid UTF-8, so `read-file`
    /// — `std::fs::read_to_string` — rejects this file outright, which is why
    /// `sha256` over a string cannot hash a release binary. If this test ever
    /// starts failing because the bytes were routed through a String, the
    /// primitive has lost its only reason to exist.
    #[test]
    fn hashes_non_utf8_bytes_that_read_file_would_reject() {
        let dir = std::env::temp_dir().join("tl-hash-binary");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("elf-ish");
        // A real ELF magic prefix plus a lone 0x80 continuation byte.
        std::fs::write(&p, [0x7f, b'E', b'L', b'F', 0x80, 0x00, 0xff]).unwrap();
        let path = p.to_str().unwrap();

        // Negative control: the existing text path genuinely cannot do this.
        assert!(
            std::fs::read_to_string(path).is_err(),
            "read_to_string accepted invalid UTF-8 — the premise of this test is gone"
        );

        let mut s = Sha256::new();
        stream_into(path, &mut s).unwrap();
        assert_eq!(hex::encode(s.finalize()).len(), 64);
    }

    /// Streaming must give the same digest as hashing the whole buffer at
    /// once, across a size that spans several `io::copy` reads.
    #[test]
    fn streamed_digest_equals_whole_buffer_digest() {
        let dir = std::env::temp_dir().join("tl-hash-large");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("blob");
        let blob: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&blob).unwrap();
        drop(f);

        let mut streamed = Sha256::new();
        stream_into(p.to_str().unwrap(), &mut streamed).unwrap();
        assert_eq!(
            hex::encode(streamed.finalize()),
            hex::encode(Sha256::digest(&blob))
        );
    }

    #[test]
    fn missing_file_is_an_error_naming_the_path() {
        let mut s = Sha256::new();
        let err = stream_into("/nonexistent/tl-hash/missing", &mut s).unwrap_err();
        assert!(err.contains("/nonexistent/tl-hash/missing"), "got: {err}");
    }
}
