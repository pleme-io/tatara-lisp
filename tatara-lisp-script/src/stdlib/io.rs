//! File I/O + stdout/stderr.
//!
//!   (print-line STR)              → STR to stdout + newline, returns nil
//!   (eprint-line STR)             → STR to stderr + newline, returns nil
//!   (read-file PATH)              → file contents as string
//!   (write-file PATH STR)         → nil (truncates if exists)
//!   (write-file-private PATH STR) → nil, file is mode 0600
//!   (path-exists? PATH)           → bool
//!   (exit CODE)                   → never returns; terminates process
//!
//! `write-file` is `std::fs::write`, so the file lands at whatever the
//! process umask allows — measured 0644. That is correct for a manifest and
//! wrong for key material, and there was no way to ask for anything else:
//! before `write-file-private` this crate contained no `chmod`, no
//! `set_permissions` and no `PermissionsExt` at all, and `tmp-dir` is 0755.
//! A script that needed to hand a private key to a child process therefore
//! had to write it world-readable first.

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
        "write-file-private",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "write-file-private", sp)?;
            let body = str_arg(&args[1], "write-file-private", sp)?;
            write_private(&path, body.as_bytes())
                .map_err(|e| EvalError::native_fn("write-file-private", e, sp))?;
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

/// Write `body` to `path` at mode 0600, creating it with that mode rather
/// than relaxing it afterwards.
///
/// Two details are load-bearing:
///
/// - `.mode(0o600)` applies only at CREATION. An existing file keeps its own
///   mode, so a second `set_permissions` is required or re-writing a path that
///   was previously 0644 silently stays 0644.
/// - The mode is set at open time and not by a later `chmod`, so there is no
///   window in which the content is on disk under a wider mode.
///
/// Non-unix has no equivalent guarantee, so it is an error rather than a
/// silent best-effort: a caller asking for 0600 is asking about a secret, and
/// writing it anyway would be the fail-open shape this primitive exists to
/// remove.
fn write_private(path: &str, body: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("{path}: {e}"))?;
        file.write_all(body).map_err(|e| format!("{path}: {e}"))?;
        file.flush().map_err(|e| format!("{path}: {e}"))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{path}: {e}"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = body;
        Err(format!(
            "{path}: mode 0600 cannot be guaranteed on this platform; \
             refusing to write rather than write a secret world-readable"
        ))
    }
}

#[cfg(all(test, unix))]
mod write_private_tests {
    use super::write_private;
    use std::os::unix::fs::PermissionsExt;

    fn mode_of(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn new_file_is_0600() {
        let dir = std::env::temp_dir().join("tl-wfp-new");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("key.asc");
        let _ = std::fs::remove_file(&p);

        write_private(p.to_str().unwrap(), b"secret").unwrap();

        assert_eq!(mode_of(&p), 0o600, "expected 0600, got {:o}", mode_of(&p));
        assert_eq!(std::fs::read(&p).unwrap(), b"secret");
    }

    /// NEGATIVE CONTROL for the reason this primitive exists: the incumbent
    /// `write-file` path leaves the file at the umask default, measured 0644.
    /// If this assertion ever fails, `std::fs::write` became safe and the
    /// primitive's justification needs re-measuring rather than trusting.
    #[test]
    fn plain_write_is_wider_than_0600() {
        let dir = std::env::temp_dir().join("tl-wfp-control");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("plain");
        let _ = std::fs::remove_file(&p);

        std::fs::write(&p, b"not secret").unwrap();

        assert_ne!(
            mode_of(&p),
            0o600,
            "std::fs::write produced 0600 on its own; the gap this primitive closes may be gone"
        );
    }

    /// `.mode()` applies only at creation, so re-writing a path that already
    /// exists at 0644 would silently keep 0644 without the explicit
    /// `set_permissions`. This is the case that makes the second call
    /// load-bearing rather than belt-and-braces.
    #[test]
    fn existing_wide_file_is_narrowed() {
        let dir = std::env::temp_dir().join("tl-wfp-existing");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("was-0644");
        std::fs::write(&p, b"old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&p), 0o644);

        write_private(p.to_str().unwrap(), b"new").unwrap();

        assert_eq!(mode_of(&p), 0o600, "an existing 0644 file was not narrowed");
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
    }

    #[test]
    fn unwritable_directory_is_an_error_naming_the_path() {
        let err = write_private("/nonexistent/tl-wfp/key", b"x").unwrap_err();
        assert!(err.contains("/nonexistent/tl-wfp/key"), "got: {err}");
    }
}
