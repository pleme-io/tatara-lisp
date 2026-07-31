//! tatara-lisp — a small homoiconic S-expression language.
//!
//! The surface is homoiconic: the *reader* produces an AST (`Sexp`) that is
//! itself S-expressions. Macros operate on `Sexp` and yield `Sexp`.
//!
//! Scope of v0 (this scaffold): lexer, reader, `Sexp` AST, environment,
//! plus a minimal evaluator shell (special forms `quote`, `if`, `let`, `lambda`).
//! The `ProcessSpec` compiler (defpoint macro + flattening to
//! `tatara_process::ProcessSpec`) lands in `compile.rs` in the next pass.
//!
//! ```lisp
//! (defpoint observability-stack
//!   :parent seph.1
//!   :class (Gate Observability Bounded Monotone Internal)
//!   :intent (nix "github:pleme-io/k8s" :attr "observability")
//!   :compliance (baseline fedramp-moderate
//!                :at-boundary (nist SC-7)
//!                :post        (cis-k8s-v1.8))
//!   :depends-on (akeyless-injection))
//! ```

// Allow the derive macro's `::tatara_lisp::...` paths to resolve when
// `#[derive(TataraDomain)]` is applied inside tatara-lisp itself
// (`CompilerSpec` + test modules).
extern crate self as tatara_lisp;

pub mod ast;
pub mod compile;
pub mod compiler_spec;
pub mod diagnostic;
pub mod domain;
pub mod env;
pub mod error;
pub mod macro_expand;
pub mod reader;
pub mod span;
pub mod spanned;
pub mod spanned_expand;

pub use compiler_spec::{
    load_from_disk, realize_in_memory, realize_to_disk, CompilerSpec, RealizedCompiler,
};
pub use domain::{
    assert_tatara_domain_well_formed, AttestHandler, AttestableDomain, ComplianceHandler,
    CompliantDomain, DependentDomain,
    DepsHandler, DocHandler, DocumentedDomain, DomainHandler, HelpDomain, HelpHandler,
    KeywordCollision, KeywordSexp, LifecycleHandler, LifecycleProtocol, ObservabilityHandler,
    ObservableDomain,
    RenderHandler, RenderableDomain, RolloutStrategy, SchemaHandler, SchematicDomain,
    StabilityHandler, StableDomain, TataraDomain, ValidateHandler, ValidatedDomain,
};
// Derive macros — same names as the traits, different namespace (procedural
// macros vs. types), so they coexist cleanly under one import.
pub use tatara_lisp_derive::{KeywordSexp as DeriveKeywordSexp, TataraDomain as DeriveTataraDomain};

pub use ast::{Atom, AtomKind, QuoteForm, Sexp, UnknownAtomKind, UnknownQuoteForm};
pub use compile::{compile_named, compile_named_from_forms, compile_typed, NamedDefinition};
pub use diagnostic::{caret_run, format_diagnostic, line_at, line_col, span_width_chars, LineCol};
pub use env::Env;
pub use error::{
    CompilerSpecIoStage, ExpectedKwargShape, KwargPath, KwargPathKind, LispError, MacroDefHead,
    OptionalParamMalformedReason, Result, SexpShape, SexpWitness, StructuralKind,
    TemplateInvariantKind, UnknownCompilerSpecIoStage, UnknownExpectedKwargShape,
    UnknownKwargPathKind, UnknownMacroDefHead, UnknownSexpShape, UnknownStructuralKind,
    UnknownUnquoteForm, UnquoteForm,
};
pub use macro_expand::{
    Expander, MacroArgCarrier, MacroDef, MacroParams, OptionalParam, ResourceLimits,
    DEFAULT_RESOURCE_LIMITS, UNBOUNDED_RESOURCE_LIMITS,
};
pub use reader::{read, read_spanned};
pub use span::Span;
pub use spanned::{Spanned, SpannedForm};
pub use spanned_expand::SpannedExpander;
