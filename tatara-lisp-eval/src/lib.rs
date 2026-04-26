//! tatara-lisp-eval — runtime evaluator for the tatara-lisp authoring surface.
//!
//! See `docs/eval-design.md` in the tatara-lisp repo for scope, FFI surface,
//! error model, and the boundary with the plain tatara-lisp compile pipeline.
//!
//! This crate extends tatara-lisp; it does not replace it. The plain `Sexp`
//! AST and `compile_typed` flow remain the fast, committed, cacheable path
//! for typed infrastructure DSLs. `tatara-lisp-eval` is for the runtime /
//! REPL / ad-hoc path — live orchestration, rule evaluation, hot-reloaded
//! diagnostic bundles.
//!
//! # Phase progress (Phase 2.2 scaffold)
//!
//! This commit establishes shape: module layout, public types, stub
//! interpreter that evaluates only literal atoms. Subsequent phases
//! (2.3-2.7) fill in special forms, FFI, REPL, errors, tests.

pub mod build_check;
pub mod channel;
pub mod code;
pub mod env;
pub mod error;
pub mod eval;
pub mod ffi;
pub mod hof;
pub mod lisp_stdlib;
pub mod map;
pub mod module;
pub mod primitive;
pub mod repl;
pub mod special;
pub mod type_check;
pub mod value;

pub use env::Env;
pub use error::{EvalError, Result};
pub use eval::Interpreter;
pub use ffi::{Arity, Caller, FromValue, HigherOrderCallable, IntoValue, NativeCallable};
pub use hof::install_hof;
pub use lisp_stdlib::install_lisp_stdlib_with;
pub use map::install_map;
pub use module::{
    FilesystemLoader, Loader, MapLoader, Module, ModuleError, ModuleRegistry, NoLoader,
};
pub use primitive::install_primitives;
pub use repl::ReplSession;
pub use value::{ErrorObj, MapKey, Value};

/// One-stop installer: registers the full battery — Rust primitives
/// (arithmetic, comparison, list, string, IO), higher-order Rust
/// primitives (apply, map, filter, foldl, foldr, reduce, find, ...),
/// and the pure-Lisp standard library (compose, pipe, ->, ->>, defflow,
/// dotimes, range, distinct, group-by helpers, etc.).
///
/// This is the recommended entry point for embedders. If you want to
/// install only a subset (e.g. primitives + hof, no Lisp stdlib),
/// call the individual installers in order: `install_primitives`,
/// `install_hof`, `install_lisp_stdlib_with`.
pub fn install_full_stdlib_with<H: 'static>(interp: &mut Interpreter<H>, host: &mut H) {
    install_primitives(interp);
    install_hof(interp);
    install_map(interp);
    channel::install_channels(interp);
    type_check::install_type_check(interp);
    install_lisp_stdlib_with(interp, host);
}

// Re-export the tatara-lisp items every embedder will need.
pub use tatara_lisp::{read, read_spanned, Sexp, Span, Spanned, SpannedExpander, SpannedForm};
