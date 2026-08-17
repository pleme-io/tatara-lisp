//! Process / shell integration.
//!
//!   (exec-check CMD ARG…)        → 0 on success, non-zero exit code otherwise
//!                                   Streams stdin/stdout/stderr to the parent.
//!   (exec-capture CMD ARG…)      → the CAPTURE RECORD (below)
//!                                   Captures stdout + stderr, exposes exit code.
//!   (exec-ok? CMD ARG…)          → bool; true iff exit code is 0
//!   (sh-exec STR)                → convenience: run STR through `sh -c`
//!                                   returning the capture-form result
//!
//! Credential-carrying variants. Use these instead of putting a secret in an
//! argument: argv is world-readable from the process table, and in CI it is
//! readable by co-tenant steps and sibling containers.
//!
//!   (exec-with-stdin IN CMD ARG…)  → capture form; IN is written to the
//!                                   child's stdin. The `--password-stdin`
//!                                   shape that docker / helm / skopeo / gh
//!                                   already support. Preferred.
//!   (exec-with-env ENV CMD ARG…)   → capture form; ENV is an alist of
//!                                   (KEY VALUE) pairs set for the child only.
//!                                   For tools with no stdin form. Weaker than
//!                                   stdin — the environment is readable by the
//!                                   same uid — but far stronger than argv.
//!
//! No implicit shell interpolation. Arguments are passed literally to
//! the underlying process; no glob / word-splitting / $VAR substitution.
//! Scripts that want shell features use `sh-exec` explicitly.
//!
//! ── THE CAPTURE RECORD ───────────────────────────────────────────────────
//! Every capture-form primitive above returns the SAME eight-field alist, so a
//! caller never has to know which form produced it:
//!
//!   (:status N)          exit code, or -1 when killed by a signal
//!   (:stdout "…")        captured stdout
//!   (:stderr "…")        captured stderr
//!   (:argv ("cmd" "a"))  the argv LIST, exact and unsplit
//!   (:program "cmd")     the program as ASKED FOR
//!   (:resolved "/nix/…") the program as RESOLVED — "" when PATH lookup failed
//!   (:cwd "/…")          the directory the child inherited
//!   (:duration-ms N)     wall-clock milliseconds
//!
//! The last five landed 2026-08-17 and are not decoration. A deshellify port's
//! correctness argument is always "the new thing does what the old thing did",
//! which is a COMPARISON — and you cannot compare an invocation you did not
//! record. Concretely: `:resolved` answers "did the RIGHT tool run" (the
//! silent-PATH-fallback class — `Command::new("kubectl")` runs whatever PATH
//! found first, and 218 such bare spawns were measured in pleme-io/forge);
//! `:cwd` distinguishes a write to a flake's read-only /nix/store source copy
//! from one to the work tree, which otherwise surfaces only as a bare
//! `Permission denied (os error 13)`; `:duration-ms` separates "failed" from
//! "hung", which a status alone cannot.
//!
//! `:argv` is a LIST and never a joined string, because re-quoting a command
//! changes it — a single string cannot be compared against what was run.
//!
//! ADDITIVE by construction: `status-of` / `stdout-of` / `stderr-of` and every
//! other `alist-get` consumer are unaffected, and nothing in the tree asserted
//! the record's length.
//!
//! Canonical technique: pleme-io/docs/controlled-subprocess.md (rung 2).

use std::process::{Command, Stdio};
use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "exec-check",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let (cmd, rest) = split_cmd(args, "exec-check", sp)?;
            let status = Command::new(&*cmd)
                .args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| EvalError::native_fn("exec-check", e.to_string(), sp))?;
            Ok(Value::Int(status.code().unwrap_or(-1) as i64))
        },
    );

    interp.register_fn(
        "exec-ok?",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let (cmd, rest) = split_cmd(args, "exec-ok?", sp)?;
            let status = Command::new(&*cmd)
                .args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| EvalError::native_fn("exec-ok?", e.to_string(), sp))?;
            Ok(Value::Bool(status.success()))
        },
    );

    interp.register_fn(
        "exec-capture",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let (cmd, rest) = split_cmd(args, "exec-capture", sp)?;
            let started = std::time::Instant::now();
            let out = Command::new(&*cmd)
                .args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::null())
                .output()
                .map_err(|e| EvalError::native_fn("exec-capture", e.to_string(), sp))?;
            let inv = Invocation::new(
                &cmd,
                rest.iter().map(|s| s.as_ref()).collect(),
                started.elapsed().as_millis(),
            );
            Ok(capture_result(&inv, &out))
        },
    );

    interp.register_fn(
        "exec-with-stdin",
        Arity::AtLeast(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            use std::io::Write;
            let payload = str_arg(&args[0], "exec-with-stdin", sp)?;
            let (cmd, rest) = split_cmd(&args[1..], "exec-with-stdin", sp)?;
            let started = std::time::Instant::now();
            let mut child = Command::new(&*cmd)
                .args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| EvalError::native_fn("exec-with-stdin", e.to_string(), sp))?;
            // `take()` so the pipe is dropped before we wait — a tool reading
            // stdin to EOF deadlocks otherwise.
            if let Some(mut sink) = child.stdin.take() {
                sink.write_all(payload.as_bytes())
                    .map_err(|e| EvalError::native_fn("exec-with-stdin", e.to_string(), sp))?;
            }
            let out = child
                .wait_with_output()
                .map_err(|e| EvalError::native_fn("exec-with-stdin", e.to_string(), sp))?;
            let inv = Invocation::new(
                &cmd,
                rest.iter().map(|s| s.as_ref()).collect(),
                started.elapsed().as_millis(),
            );
            Ok(capture_result(&inv, &out))
        },
    );

    interp.register_fn(
        "exec-with-env",
        Arity::AtLeast(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let pairs = env_pairs(&args[0], "exec-with-env", sp)?;
            let (cmd, rest) = split_cmd(&args[1..], "exec-with-env", sp)?;
            let mut c = Command::new(&*cmd);
            c.args(rest.iter().map(|s| s.as_ref()))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in &pairs {
                c.env(k.as_ref(), v.as_ref());
            }
            let started = std::time::Instant::now();
            let out = c
                .output()
                .map_err(|e| EvalError::native_fn("exec-with-env", e.to_string(), sp))?;
            let inv = Invocation::new(
                &cmd,
                rest.iter().map(|s| s.as_ref()).collect(),
                started.elapsed().as_millis(),
            );
            Ok(capture_result(&inv, &out))
        },
    );

    interp.register_fn(
        "sh-exec",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let script = str_arg(&args[0], "sh-exec", sp)?;
            let started = std::time::Instant::now();
            let out = Command::new("sh")
                .arg("-c")
                .arg(&*script)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| EvalError::native_fn("sh-exec", e.to_string(), sp))?;
            // argv is recorded as the REAL three-element form `sh -c <script>`,
            // not as the script text alone. That is the honest record: sh-exec
            // is the one primitive that hands a string to a shell, and the
            // record should show that a shell was involved.
            let inv = Invocation::new("sh", vec!["-c", &script], started.elapsed().as_millis());
            Ok(capture_result(&inv, &out))
        },
    );
}

fn split_cmd(
    args: &[Value],
    fname: &'static str,
    sp: tatara_lisp::Span,
) -> Result<(Arc<str>, Vec<Arc<str>>), EvalError> {
    let mut it = args.iter();
    let cmd = str_arg(
        it.next().ok_or_else(|| {
            EvalError::native_fn(fname, "expected at least 1 argument".to_string(), sp)
        })?,
        fname,
        sp,
    )?;
    let rest = it
        .map(|v| str_arg(v, fname, sp))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((cmd, rest))
}

/// Read an alist of `(KEY VALUE)` string pairs.
///
/// Rejects a malformed entry rather than skipping it: a silently-dropped pair
/// would run the child WITHOUT the credential and report success, which is a
/// worse failure than an error.
fn env_pairs(
    v: &Value,
    fname: &'static str,
    sp: tatara_lisp::Span,
) -> Result<Vec<(Arc<str>, Arc<str>)>, EvalError> {
    let items = match v {
        Value::List(items) => items,
        _ => {
            return Err(EvalError::native_fn(
                fname,
                "first argument must be an alist of (KEY VALUE) pairs".to_string(),
                sp,
            ));
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for it in items.iter() {
        match it {
            Value::List(kv) if kv.len() == 2 => {
                out.push((str_arg(&kv[0], fname, sp)?, str_arg(&kv[1], fname, sp)?));
            }
            _ => {
                return Err(EvalError::native_fn(
                    fname,
                    "each env entry must be a 2-element (KEY VALUE) list".to_string(),
                    sp,
                ));
            }
        }
    }
    Ok(out)
}

/// What was actually invoked, carried alongside the output so the record can
/// answer WHICH binary ran and WHERE — not just what it printed.
///
/// ── ★ WHY THE RECORD NEEDED WIDENING ─────────────────────────────────────
/// The record used to be `{status, stdout, stderr}`: three fields built from
/// `std::process::Output` alone, so argv, the resolved binary and the cwd were
/// not omitted by choice — [`capture_result`] never received them.
///
/// That gap is what makes a deshellify port's correctness argument uncheckable.
/// The argument is always "the new thing does what the old thing did", and that
/// is a COMPARISON; you cannot compare an invocation you did not record. Two
/// concrete failure classes it left invisible:
///
///   * WHICH BINARY. `Command::new("kubectl")` runs whatever `PATH` resolved
///     first, so a verdict gets attributed to a binary nobody declared — the
///     silent-PATH-fallback class. With no resolved-path field, "did the right
///     tool run" was unanswerable from the record and could only be settled by
///     reading source. Measured in pleme-io/forge: 218 bare-literal spawns.
///   * WHERE. A bump that targets a flake's read-only `/nix/store` source copy
///     instead of the work tree fails with a bare `Permission denied (os error
///     13)`; the cwd is the field that says which one it was.
///
/// `duration-ms` separates "failed" from "hung", which a status alone cannot.
struct Invocation<'a> {
    program: &'a str,
    args: Vec<&'a str>,
    elapsed_ms: u128,
}

impl<'a> Invocation<'a> {
    fn new(program: &'a str, args: Vec<&'a str>, elapsed_ms: u128) -> Self {
        Self {
            program,
            args,
            elapsed_ms,
        }
    }
}

/// Resolve `program` the way the OS just did, so the record names the binary
/// that actually ran rather than the string we asked for.
///
/// A path-bearing program is already unambiguous and is returned as-is. A bare
/// name is resolved through `PATH`; when resolution fails the field is the empty
/// string — which is a FINDING (the spawn succeeded, so something ran, and we
/// could not name it) and deliberately not the bare name again, because that
/// would render "resolved" and "unresolved" as the same bytes.
fn resolved_program(program: &str) -> String {
    if program.contains(std::path::MAIN_SEPARATOR) {
        return program.to_string();
    }
    which::which(program)
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn capture_result(inv: &Invocation<'_>, out: &std::process::Output) -> Value {
    // argv as a LIST, never a joined string: re-quoting a command changes it,
    // so a single string cannot be compared against what was run.
    let mut argv = vec![Value::Str(Arc::from(inv.program))];
    argv.extend(inv.args.iter().map(|a| Value::Str(Arc::from(*a))));

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    Value::list(vec![
        Value::list(vec![
            Value::Keyword(Arc::from("status")),
            Value::Int(out.status.code().unwrap_or(-1) as i64),
        ]),
        Value::list(vec![
            Value::Keyword(Arc::from("stdout")),
            Value::Str(Arc::from(String::from_utf8_lossy(&out.stdout).as_ref())),
        ]),
        Value::list(vec![
            Value::Keyword(Arc::from("stderr")),
            Value::Str(Arc::from(String::from_utf8_lossy(&out.stderr).as_ref())),
        ]),
        Value::list(vec![Value::Keyword(Arc::from("argv")), Value::list(argv)]),
        Value::list(vec![
            Value::Keyword(Arc::from("program")),
            Value::Str(Arc::from(inv.program)),
        ]),
        Value::list(vec![
            Value::Keyword(Arc::from("resolved")),
            Value::Str(Arc::from(resolved_program(inv.program).as_str())),
        ]),
        Value::list(vec![
            Value::Keyword(Arc::from("cwd")),
            Value::Str(Arc::from(cwd.as_str())),
        ]),
        Value::list(vec![
            Value::Keyword(Arc::from("duration-ms")),
            Value::Int(inv.elapsed_ms.min(i64::MAX as u128) as i64),
        ]),
    ])
}
