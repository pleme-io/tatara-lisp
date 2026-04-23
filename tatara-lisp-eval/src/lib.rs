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

pub mod env;
pub mod error;
pub mod eval;
pub mod ffi;
pub mod primitive;
pub mod repl;
pub mod special;
pub mod value;

pub use env::Env;
pub use error::{EvalError, Result};
pub use eval::Interpreter;
pub use ffi::{Arity, NativeCallable};
pub use repl::ReplSession;
pub use value::Value;

// Re-export the tatara-lisp items every embedder will need.
pub use tatara_lisp::{read, read_spanned, Sexp, Span, Spanned, SpannedForm};
