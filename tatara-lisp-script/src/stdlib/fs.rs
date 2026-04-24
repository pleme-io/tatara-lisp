//! Filesystem primitives — directories, globs, metadata, temp files.
//!
//!   (glob PATTERN)              → list of paths matching (e.g. "src/**/*.rs")
//!   (walk-dir PATH)             → flat list of every file under PATH
//!   (ls PATH)                   → list of entries (files + dirs) directly inside
//!   (mkdir PATH)                → nil (fails silently if exists)
//!   (mkdir-p PATH)              → nil; creates all intermediate dirs
//!   (rm PATH)                   → nil; deletes file
//!   (rm-rf PATH)                → nil; deletes recursively
//!   (cwd)                       → current working directory
//!   (chdir PATH)                → nil; changes cwd
//!   (path-join A B …)           → joined path string
//!   (path-basename PATH)        → last path component
//!   (path-dirname PATH)         → parent dir
//!   (path-extension PATH)       → extension without leading dot
//!   (path-absolute PATH)        → absolute canonical path
//!   (file-size PATH)            → bytes
//!   (is-dir? PATH)              → bool
//!   (is-file? PATH)             → bool
//!   (tmp-dir)                   → fresh temp dir (auto-cleaned by OS)
//!   (tmp-file)                  → fresh temp file path

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    // ── glob / walk ──────────────────────────────────────────────
    interp.register_fn(
        "glob",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let pattern = str_arg(&args[0], "glob", sp)?;
            let entries = simple_glob(&pattern)
                .map_err(|e| EvalError::native_fn("glob", e, sp))?;
            Ok(Value::list(
                entries
                    .into_iter()
                    .map(|p| Value::Str(Arc::from(p.to_string_lossy().into_owned())))
                    .collect::<Vec<_>>(),
            ))
        },
    );

    interp.register_fn(
        "walk-dir",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let root = str_arg(&args[0], "walk-dir", sp)?;
            let mut out = Vec::new();
            walk_collect(Path::new(&*root), &mut out)
                .map_err(|e| EvalError::native_fn("walk-dir", e.to_string(), sp))?;
            Ok(Value::list(
                out.into_iter()
                    .map(|p| Value::Str(Arc::from(p.to_string_lossy().into_owned())))
                    .collect::<Vec<_>>(),
            ))
        },
    );

    interp.register_fn(
        "ls",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let dir = str_arg(&args[0], "ls", sp)?;
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&*dir)
                .map_err(|e| EvalError::native_fn("ls", format!("{dir}: {e}"), sp))?
                .filter_map(|r| r.ok().map(|e| e.path()))
                .collect();
            entries.sort();
            Ok(Value::list(
                entries
                    .into_iter()
                    .map(|p| Value::Str(Arc::from(p.to_string_lossy().into_owned())))
                    .collect::<Vec<_>>(),
            ))
        },
    );

    // ── create / delete ──────────────────────────────────────────
    interp.register_fn(
        "mkdir",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "mkdir", sp)?;
            match std::fs::create_dir(&*path) {
                Ok(()) => Ok(Value::Nil),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(Value::Nil),
                Err(e) => Err(EvalError::native_fn("mkdir", format!("{path}: {e}"), sp)),
            }
        },
    );

    interp.register_fn(
        "mkdir-p",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "mkdir-p", sp)?;
            std::fs::create_dir_all(&*path)
                .map_err(|e| EvalError::native_fn("mkdir-p", format!("{path}: {e}"), sp))?;
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "rm",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "rm", sp)?;
            std::fs::remove_file(&*path)
                .map_err(|e| EvalError::native_fn("rm", format!("{path}: {e}"), sp))?;
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "rm-rf",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "rm-rf", sp)?;
            if Path::new(&*path).is_dir() {
                std::fs::remove_dir_all(&*path)
                    .map_err(|e| EvalError::native_fn("rm-rf", format!("{path}: {e}"), sp))?;
            } else if Path::new(&*path).exists() {
                std::fs::remove_file(&*path)
                    .map_err(|e| EvalError::native_fn("rm-rf", format!("{path}: {e}"), sp))?;
            }
            Ok(Value::Nil)
        },
    );

    // ── cwd / chdir ──────────────────────────────────────────────
    interp.register_fn(
        "cwd",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let d = std::env::current_dir()
                .map_err(|e| EvalError::native_fn("cwd", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(d.to_string_lossy().into_owned())))
        },
    );

    interp.register_fn(
        "chdir",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "chdir", sp)?;
            std::env::set_current_dir(&*path)
                .map_err(|e| EvalError::native_fn("chdir", format!("{path}: {e}"), sp))?;
            Ok(Value::Nil)
        },
    );

    // ── path ops ─────────────────────────────────────────────────
    interp.register_fn(
        "path-join",
        Arity::AtLeast(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let mut buf = PathBuf::new();
            for v in args {
                let s = str_arg(v, "path-join", sp)?;
                buf.push(&*s);
            }
            Ok(Value::Str(Arc::from(buf.to_string_lossy().into_owned())))
        },
    );

    interp.register_fn(
        "path-basename",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let p = str_arg(&args[0], "path-basename", sp)?;
            let base = Path::new(&*p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Value::Str(Arc::from(base)))
        },
    );

    interp.register_fn(
        "path-dirname",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let p = str_arg(&args[0], "path-dirname", sp)?;
            let dir = Path::new(&*p)
                .parent()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Value::Str(Arc::from(dir)))
        },
    );

    interp.register_fn(
        "path-extension",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let p = str_arg(&args[0], "path-extension", sp)?;
            let ext = Path::new(&*p)
                .extension()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Value::Str(Arc::from(ext)))
        },
    );

    interp.register_fn(
        "path-absolute",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let p = str_arg(&args[0], "path-absolute", sp)?;
            let abs = std::fs::canonicalize(&*p)
                .map_err(|e| EvalError::native_fn("path-absolute", format!("{p}: {e}"), sp))?;
            Ok(Value::Str(Arc::from(abs.to_string_lossy().into_owned())))
        },
    );

    // ── metadata ─────────────────────────────────────────────────
    interp.register_fn(
        "file-size",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let p = str_arg(&args[0], "file-size", sp)?;
            let meta = std::fs::metadata(&*p)
                .map_err(|e| EvalError::native_fn("file-size", format!("{p}: {e}"), sp))?;
            Ok(Value::Int(meta.len() as i64))
        },
    );

    interp.register_fn(
        "is-dir?",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let p = str_arg(&args[0], "is-dir?", sp)?;
            Ok(Value::Bool(Path::new(&*p).is_dir()))
        },
    );

    interp.register_fn(
        "is-file?",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let p = str_arg(&args[0], "is-file?", sp)?;
            Ok(Value::Bool(Path::new(&*p).is_file()))
        },
    );

    // ── temp ─────────────────────────────────────────────────────
    interp.register_fn(
        "tmp-dir",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let base = std::env::temp_dir();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = base.join(format!("tatara-script-{now:x}"));
            std::fs::create_dir_all(&dir)
                .map_err(|e| EvalError::native_fn("tmp-dir", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(dir.to_string_lossy().into_owned())))
        },
    );

    interp.register_fn(
        "tmp-file",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let base = std::env::temp_dir();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = base.join(format!("tatara-script-{now:x}.tmp"));
            std::fs::write(&path, b"")
                .map_err(|e| EvalError::native_fn("tmp-file", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(path.to_string_lossy().into_owned())))
        },
    );
}

/// Walk a directory tree, collecting every file (not directories).
fn walk_collect(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(cur) = stack.pop() {
        if cur.is_file() {
            out.push(cur);
            continue;
        }
        if !cur.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&cur)? {
            let entry = entry?;
            stack.push(entry.path());
        }
    }
    Ok(())
}

/// Minimal glob engine supporting `*` (non-slash) and `**` (recursive).
/// Patterns are absolute or relative; relative patterns resolve against
/// the current directory.
fn simple_glob(pattern: &str) -> Result<Vec<PathBuf>, String> {
    let (prefix, remainder) = split_glob_prefix(pattern);
    let base = if prefix.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(&prefix)
    };
    let parts: Vec<&str> = remainder.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Ok(vec![base]);
    }
    let mut out = Vec::new();
    walk_glob(&base, &parts, 0, &mut out);
    Ok(out)
}

fn split_glob_prefix(pattern: &str) -> (String, String) {
    // Return (literal_prefix, glob_tail). Literal prefix is the path up
    // to the first component containing `*`.
    let mut literal = String::new();
    let mut rest = String::new();
    let mut found_glob = false;
    for (i, component) in pattern.split('/').enumerate() {
        if !found_glob && !component.contains('*') {
            if i > 0 && !literal.is_empty() {
                literal.push('/');
            }
            literal.push_str(component);
        } else {
            found_glob = true;
            if !rest.is_empty() {
                rest.push('/');
            }
            rest.push_str(component);
        }
    }
    if literal.is_empty() && !found_glob {
        literal = pattern.to_string();
    }
    (literal, rest)
}

fn walk_glob(dir: &Path, parts: &[&str], idx: usize, out: &mut Vec<PathBuf>) {
    if idx >= parts.len() {
        out.push(dir.to_path_buf());
        return;
    }
    let pat = parts[idx];
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    if pat == "**" {
        // Match zero or more directory levels.
        walk_glob(dir, parts, idx + 1, out);
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk_glob(&p, parts, idx, out);
            }
        }
        return;
    }
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if glob_match(pat, &name_s) {
            let p = entry.path();
            if idx + 1 == parts.len() {
                out.push(p);
            } else if p.is_dir() {
                walk_glob(&p, parts, idx + 1, out);
            }
        }
    }
}

fn glob_match(pattern: &str, name: &str) -> bool {
    // Simple `*` semantics: matches any sequence of characters except /.
    let mut pi = 0;
    let mut ni = 0;
    let pbytes = pattern.as_bytes();
    let nbytes = name.as_bytes();
    let mut star: Option<(usize, usize)> = None;
    while ni < nbytes.len() {
        if pi < pbytes.len() && (pbytes[pi] == b'?' || pbytes[pi] == nbytes[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pbytes.len() && pbytes[pi] == b'*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, ni));
        } else {
            return false;
        }
    }
    while pi < pbytes.len() && pbytes[pi] == b'*' {
        pi += 1;
    }
    pi == pbytes.len()
}
