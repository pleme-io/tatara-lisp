//! tatara-lisp-script — scripting surface for tatara-lisp.
//!
//! Wraps `tatara-lisp-eval::Interpreter<ScriptCtx>` with a batteries-included
//! stdlib (http, json, yaml, sops, file I/O, env, sha256, string ops) so a
//! `.tlisp` file can replace a bash script. The binary (`tatara-script`)
//! parses a .tlisp file, expands macros via tatara-lisp, and evaluates each
//! form against this stdlib.
//!
//! # Usage from nix-run
//!
//! ```nix
//! apps.tatara-script = {
//!   type = "app";
//!   program = "${tataraScript}/bin/tatara-script path/to/script.tlisp";
//! };
//! ```
//!
//! # Library surface
//!
//! Embedders that want to add domain-specific FFI on top of the stdlib can:
//!
//! ```rust,ignore
//! use tatara_lisp_script::{Interpreter, ScriptCtx, install_stdlib};
//!
//! let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
//! install_stdlib(&mut interp);
//! // Register more fns before eval_program.
//! ```

pub mod script_ctx;
pub mod stdlib;

pub use script_ctx::ScriptCtx;
pub use stdlib::install_stdlib;

// Re-export the evaluator so embedders don't have to depend on tatara-lisp-eval
// directly.
pub use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};
pub use tatara_lisp::{read_spanned, Spanned};

/// Convenience: read + evaluate a tatara-lisp source string against a fresh
/// interpreter with the full stdlib installed.
///
/// Primarily for tests and one-liner invocations. Binary entry points
/// should construct the `Interpreter` directly to keep the host context
/// available across calls.
pub fn eval_str(src: &str) -> Result<Value, anyhow::Error> {
    let forms = read_spanned(src)
        .map_err(|e| anyhow::anyhow!("parse error: {e}"))?;
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    install_stdlib(&mut interp);
    let mut ctx = ScriptCtx::default();
    interp
        .eval_program(&forms, &mut ctx)
        .map_err(|e| anyhow::anyhow!("eval error: {e:?}"))
}
