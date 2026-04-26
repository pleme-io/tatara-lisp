//! Module system — `(require "path.tlisp")`.
//!
//! Evaluates another .tlisp file in the current interpreter, exposing
//! its top-level `(define …)` forms as globals in the caller. Relative
//! paths resolve against the directory of the *current* file being
//! evaluated (or cwd if there isn't one — e.g. from a REPL).
//!
//! The require cache canonicalizes paths before storing so that
//! `(require "./util.tlisp")` and `(require "../scripts/util.tlisp")`
//! from sibling files converge on the same entry — no double-eval.
//!
//! Implementation note: because tatara-lisp-eval does not expose an
//! "eval a new form inside my interpreter" from the FFI layer, we
//! install `require` as a **macro-like** native fn that READS + EXPANDS
//! the target file but hands the forms back to the caller as a list
//! the caller will then have to splice. To keep this ergonomic we
//! wrap the top-level require behavior behind the
//! `install_require_with` API (called from main.rs), which takes the
//! interpreter by pointer and re-enters eval_program.

use std::path::{Path, PathBuf};

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;

/// Install `(require PATH)` on the given interpreter. Because `require`
/// has to re-enter evaluation on the same interpreter, we capture an
/// `Arc<Mutex<Interpreter>>` (or equivalent) via a closure — but since
/// `Interpreter<H>` is not `Send+Sync`, we instead install a stub here
/// and let main.rs override it with the real function after the
/// interpreter is constructed.
///
/// This stub exists so `install_stdlib` can be called without a ready
/// interpreter-to-self reference; scripts get a clear error if they
/// call `(require)` without the real binding having been patched in.
pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "require",
        Arity::Exact(1),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            Err(EvalError::native_fn(
                "require",
                "require is only available when the interpreter is driven \
                 by `tatara-script` (or an embedder that installs the \
                 require hook). See tatara-lisp-script/src/require_hook.rs.",
                sp,
            ))
        },
    );
}

/// Resolve a `(require)` target string into an absolute canonical path.
/// Relative paths resolve against the directory of `ctx.current_file`,
/// or `cwd` if that's unset. Absolute paths pass through unchanged.
pub fn resolve_require_path(ctx: &ScriptCtx, target: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(target);
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if let Some(current) = ctx.current_file.as_ref().and_then(|p| p.parent()) {
        current.join(candidate)
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cwd: {e}"))?
            .join(candidate)
    };
    std::fs::canonicalize(&absolute).map_err(|e| format!("require {absolute:?}: {e}"))
}

/// Utility for main.rs: take a `(require PATH)` string, resolve +
/// canonicalize, return None if already required (caller should no-op),
/// or the canonical path otherwise.
pub fn plan_require(ctx: &mut ScriptCtx, target: &str) -> Result<Option<PathBuf>, String> {
    let canonical = resolve_require_path(ctx, target)?;
    if ctx.required.contains(&canonical) {
        return Ok(None);
    }
    ctx.required.insert(canonical.clone());
    Ok(Some(canonical))
}

/// Convenience wrapper: given an already-resolved path, read + parse it
/// and return the spanned forms, ready for the caller to `eval_program`.
pub fn read_forms(path: &Path) -> Result<Vec<tatara_lisp::Spanned>, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    tatara_lisp::read_spanned(&src).map_err(|e| format!("parse {path:?}: {e:?}"))
}
