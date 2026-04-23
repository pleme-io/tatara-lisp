//! Runtime evaluator errors.
//!
//! Every variant carries a `Span` pointing back to the offending source
//! subform (or `Span::synthetic()` when the error originated in macro-
//! generated code or native fn). No panics from the evaluator itself —
//! panics from registered native fns are caught at the FFI boundary and
//! surfaced here as `EvalError::NativeFn`.

use std::sync::Arc;

use tatara_lisp::Span;
use thiserror::Error;

use crate::ffi::Arity;

pub type Result<T> = std::result::Result<T, EvalError>;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("unbound symbol: {name} at {at}")]
    UnboundSymbol { name: Arc<str>, at: Span },

    #[error("arity mismatch in {fn_name}: expected {expected:?}, got {got} at {at}")]
    ArityMismatch {
        fn_name: Arc<str>,
        expected: Arity,
        got: usize,
        at: Span,
    },

    #[error("type mismatch: expected {expected}, got {got} at {at}")]
    TypeMismatch {
        expected: &'static str,
        got: &'static str,
        at: Span,
    },

    #[error("division by zero at {at}")]
    DivisionByZero { at: Span },

    #[error("not callable: value of type {value_kind} at {at}")]
    NotCallable { value_kind: &'static str, at: Span },

    #[error("bad special form `{form}`: {reason} at {at}")]
    BadSpecialForm {
        form: Arc<str>,
        reason: String,
        at: Span,
    },

    #[error("in native fn {name}: {reason} at {at}")]
    NativeFn {
        name: Arc<str>,
        reason: String,
        at: Span,
    },

    #[error("reader error: {0}")]
    Reader(#[from] tatara_lisp::LispError),

    #[error("halted (host-initiated interrupt)")]
    Halted,

    #[error("not yet implemented: {0} (Phase 2.3+)")]
    NotImplemented(&'static str),
}

impl EvalError {
    pub fn unbound(name: impl Into<Arc<str>>, at: Span) -> Self {
        Self::UnboundSymbol {
            name: name.into(),
            at,
        }
    }

    pub fn type_mismatch(expected: &'static str, got: &'static str, at: Span) -> Self {
        Self::TypeMismatch { expected, got, at }
    }

    pub fn native_fn(name: impl Into<Arc<str>>, reason: impl Into<String>, at: Span) -> Self {
        Self::NativeFn {
            name: name.into(),
            reason: reason.into(),
            at,
        }
    }

    pub fn bad_form(form: impl Into<Arc<str>>, reason: impl Into<String>, at: Span) -> Self {
        Self::BadSpecialForm {
            form: form.into(),
            reason: reason.into(),
            at,
        }
    }

    /// The span this error is attached to, if any.
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::UnboundSymbol { at, .. }
            | Self::ArityMismatch { at, .. }
            | Self::TypeMismatch { at, .. }
            | Self::DivisionByZero { at }
            | Self::NotCallable { at, .. }
            | Self::BadSpecialForm { at, .. }
            | Self::NativeFn { at, .. } => Some(*at),
            Self::Reader(_) | Self::Halted | Self::NotImplemented(_) => None,
        }
    }
}
