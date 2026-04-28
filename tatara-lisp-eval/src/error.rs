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

    /// A Lisp-side error raised via `(throw ...)`. Caught by
    /// `(try ... (catch (e) ...))`. The carried `Value` is whatever
    /// the user threw — conventionally a `Value::Error` produced by
    /// `(error ...)` / `(ex-info ...)`, but any Value is allowed.
    #[error("user error: {value}")]
    User {
        value: crate::value::Value,
        at: Span,
    },
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
            | Self::NativeFn { at, .. }
            | Self::User { at, .. } => Some(*at),
            Self::Reader(_) | Self::Halted | Self::NotImplemented(_) => None,
        }
    }

    /// Render this error with source context — finds the line containing
    /// the error's span in `src`, prints that line, and underlines the
    /// span with `^` markers. Produces a multi-line string suitable for
    /// CLI / REPL output.
    ///
    /// If the error has no span, or its span is synthetic, renders just
    /// the error message without source context.
    pub fn render(&self, src: &str) -> String {
        let Some(span) = self.span() else {
            return self.to_string();
        };
        if span.is_synthetic() || span.end > src.len() {
            return self.to_string();
        }

        let (line_no, col) = Span::line_col(src, span.start);
        let line = find_line(src, span.start);
        let line_num_str = format!("{line_no}");
        let gutter = " ".repeat(line_num_str.len());

        let col_offset = col.saturating_sub(1);
        let len = (span.end - span.start).max(1);
        let caret_line = format!(
            "{gutter} | {blanks}{carets}",
            blanks = " ".repeat(col_offset),
            carets = "^".repeat(len)
        );

        let summary = self.short_message();
        format!(
            "error: {summary}\n  at line {line_no}, column {col}\n{line_num_str} | {line}\n{caret_line}",
        )
    }

    /// Short, one-line summary of the error kind — no source context.
    pub fn short_message(&self) -> String {
        match self {
            Self::UnboundSymbol { name, .. } => format!("unbound symbol `{name}`"),
            Self::ArityMismatch {
                fn_name,
                expected,
                got,
                ..
            } => format!("`{fn_name}` expected {expected:?}, got {got}"),
            Self::TypeMismatch { expected, got, .. } => {
                format!("type mismatch: expected {expected}, got {got}")
            }
            Self::DivisionByZero { .. } => "division by zero".into(),
            Self::NotCallable { value_kind, .. } => {
                format!("value of type {value_kind} is not callable")
            }
            Self::BadSpecialForm { form, reason, .. } => {
                format!("bad `{form}`: {reason}")
            }
            Self::NativeFn { name, reason, .. } => format!("in native `{name}`: {reason}"),
            Self::Reader(e) => format!("reader: {e}"),
            Self::Halted => "halted".into(),
            Self::NotImplemented(what) => format!("not yet implemented: {what}"),
            Self::User { value, .. } => format!("uncaught: {value}"),
        }
    }
}

/// Extract the single line of `src` containing the byte offset `pos`.
fn find_line(src: &str, pos: usize) -> &str {
    let start = src[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = src[pos..].find('\n').map(|i| pos + i).unwrap_or(src.len());
    &src[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_line_single_line() {
        let src = "foo bar baz";
        assert_eq!(find_line(src, 5), "foo bar baz");
    }

    #[test]
    fn find_line_multi_line() {
        let src = "aaa\nbbb\nccc";
        assert_eq!(find_line(src, 0), "aaa");
        assert_eq!(find_line(src, 4), "bbb");
        assert_eq!(find_line(src, 8), "ccc");
    }

    #[test]
    fn render_includes_line_col_and_caret() {
        let err = EvalError::unbound("foo", Span::new(4, 7));
        let src = "(+ x foo y)";
        let rendered = err.render(src);
        assert!(rendered.contains("unbound symbol `foo`"));
        assert!(rendered.contains("line 1, column 5"));
        assert!(rendered.contains("(+ x foo y)"));
        assert!(rendered.contains("^^^"));
    }

    #[test]
    fn render_without_span_falls_back_to_display() {
        let err = EvalError::Halted;
        assert!(!err.render("ignored").is_empty());
    }

    #[test]
    fn render_synthetic_span_falls_back() {
        let err = EvalError::unbound("x", Span::synthetic());
        let rendered = err.render("some source");
        // No source context when span is synthetic.
        assert!(!rendered.contains("line"));
    }

    #[test]
    fn short_message_for_each_variant() {
        use crate::ffi::Arity;

        assert!(EvalError::DivisionByZero {
            at: Span::synthetic(),
        }
        .short_message()
        .contains("division"));

        assert!(EvalError::unbound("foo", Span::synthetic())
            .short_message()
            .contains("foo"));

        assert!(EvalError::ArityMismatch {
            fn_name: "+".into(),
            expected: Arity::Exact(2),
            got: 3,
            at: Span::synthetic(),
        }
        .short_message()
        .contains("got 3"));
    }
}
