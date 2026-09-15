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
//! let mut ctx = ScriptCtx::default();
//! install_stdlib(&mut interp, &mut ctx);
//! // Register more fns before eval_program.
//! ```

/// Owned temp-path registry — makes the `(tmp-dir)` / `(tmp-file)` leak
/// unrepresentable. See the module docs for the incident it closes.
pub mod scratch;
pub mod script_ctx;
pub mod stdlib;

pub use script_ctx::ScriptCtx;
pub use stdlib::install_stdlib;

// Re-export the evaluator so embedders don't have to depend on tatara-lisp-eval
// directly.
pub use tatara_lisp::{read_spanned, Spanned};
pub use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

/// Convenience: read + evaluate a tatara-lisp source string against a fresh
/// interpreter with the full stdlib installed.
///
/// Primarily for tests and one-liner invocations. Binary entry points
/// should construct the `Interpreter` directly to keep the host context
/// available across calls.
pub fn eval_str(src: &str) -> Result<Value, anyhow::Error> {
    let forms = read_spanned(src).map_err(|e| anyhow::anyhow!("parse error: {e}"))?;
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    let mut ctx = ScriptCtx::default();
    install_stdlib(&mut interp, &mut ctx);
    interp
        .eval_program(&forms, &mut ctx)
        .map_err(|e| anyhow::anyhow!("eval error: {e:?}"))
}

/// Every name a script may call on THIS binary: special forms, macros, and
/// every top-level global the real stdlib installs.
///
/// ── ONE SOURCE, TWO READERS ──────────────────────────────────────────────
/// `tatara-script lint`'s `unbound-symbol` rule and `tatara-script symbols`
/// both read this. They must never disagree: if `symbols` listed a name the
/// lint rejects (or the reverse), the listing would send an author to a name
/// that fails. So there is exactly one computation, and it comes from a real
/// interpreter with the real stdlib — never a hand-kept table, which would
/// drift the first time a primitive landed.
///
/// ── WHY `symbols` EXISTS ─────────────────────────────────────────────────
/// The lint's own diagnostic says "check the spelling against the installed
/// primitives", but until this there was no way to LIST them; `--help` pointed
/// at crate docs unreachable from the binary. The cost was measured
/// 2026-09-15: `member?` was hand-rolled ~10 times under 5 names across the
/// fleet while being a builtin the whole time, and
/// `hardened-images/tools/classic-crypto-gate.tlisp:77` records probing for a
/// function by trial and error. Authors re-derive what they cannot see.
///
/// A `BTreeSet`, so the output is sorted and duplicate-free:
/// `reserved_head_names` already folds in top-level globals, and the lint's
/// original construction added them a second time.
#[must_use]
pub fn bound_symbol_names() -> std::collections::BTreeSet<String> {
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    let mut ctx = ScriptCtx::with_argv(Vec::<String>::new());
    install_stdlib(&mut interp, &mut ctx);
    let mut names: std::collections::BTreeSet<String> = interp
        .reserved_head_names()
        .iter()
        .map(ToString::to_string)
        .collect();
    names.extend(
        interp
            .globals_snapshot()
            .iter_top_level()
            .into_iter()
            .map(|(name, _)| name.to_string()),
    );
    names
}
