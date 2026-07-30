//! Runtime evaluator errors.
//!
//! Every variant carries a `Span` pointing back to the offending source
//! subform (or `Span::synthetic()` when the error originated in macro-
//! generated code or native fn). No panics from the evaluator itself —
//! panics from registered native fns are caught at the FFI boundary and
//! surfaced here as `EvalError::NativeFn`.

use std::sync::Arc;

use tatara_lisp::{caret_run, line_at, span_width_chars, Span};
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
    ///
    /// The caret line comes from [`tatara_lisp::caret_run`] — the fleet's
    /// ONE caret renderer — rather than the hand-rolled pad this method
    /// used to carry. That lift fixed two bugs the local copy had drifted
    /// into, both of them mis-placing the underline relative to the span
    /// it names:
    ///
    /// * it padded with `" ".repeat(col - 1)`, so a tab-indented source
    ///   line put one space where the source spent a whole tab-stop and
    ///   the underline slid left of its span;
    /// * it sized the underline as `span.end - span.start`, a BYTE count,
    ///   while padding to a CHAR column — so a multi-byte subform drew
    ///   more carets than it occupies columns.
    ///
    /// `pending-caret-multiline`: a span covering more than one line still
    /// draws its full char-width of carets under the single line rendered
    /// above it, overflowing that line's end. Both pre-lift copies did
    /// this and the shared renderer preserves it deliberately — clamping
    /// the run to the rendered line is a real output change for every
    /// whole-form span (which is most of them), so it wants its own
    /// measured pass rather than riding along inside this consolidation.
    pub fn render(&self, src: &str) -> String {
        let Some(span) = self.span() else {
            return self.to_string();
        };
        if span.is_synthetic() || span.end > src.len() {
            return self.to_string();
        }

        let (line_no, col) = Span::line_col(src, span.start);
        let line = line_at(src, span.start);
        let line_num_str = format!("{line_no}");
        let gutter = " ".repeat(line_num_str.len());

        // CHARS, not bytes — `caret_run` pads to a char column, so the
        // width must be counted in the same unit or the two disagree on
        // any non-ASCII source. That conversion is
        // [`tatara_lisp::span_width_chars`], the same one
        // `format_diagnostic` goes through, rather than a second copy of
        // the expression here: two hand-rolls of a unit conversion are
        // exactly how the byte-vs-char drift this comment describes got
        // in. It also carries the off-char-boundary degrade-to-1 guard,
        // so a diagnostic renderer never panics.
        let caret_line = format!(
            "{gutter} | {run}",
            run = caret_run(line, col, span_width_chars(src, span))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_slices_the_span_s_own_line_via_the_shared_slicer() {
        // Replaces the deleted local `find_line`, which duplicated
        // `tatara_lisp::line_at`. Pinned through `render` rather than by
        // calling the slicer directly, so the assertion is about the
        // artifact operators actually read.
        let src = "aaa\nbbb\nccc";
        let rendered = EvalError::unbound("bbb", Span::new(4, 7)).render(src);
        assert!(
            rendered.contains("2 | bbb"),
            "line 2 must be the rendered snippet, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("aaa") && !rendered.contains("ccc"),
            "only the span's own line may be rendered, got:\n{rendered}"
        );
    }

    #[test]
    fn render_mirrors_a_tab_indent_into_the_caret_pad() {
        // BUG FIX PIN. Pre-lift this method padded with
        // `" ".repeat(col - 1)`, so `\tfoo` rendered its underline after
        // ONE space while the source line spent a whole tab-stop — the
        // carets landed left of `foo` on every real terminal. Going
        // through the shared `caret_run` mirrors the tab through.
        let src = "\tfoo";
        let rendered = EvalError::unbound("foo", Span::new(1, 4)).render(src);
        assert!(
            rendered.contains("\t^^^"),
            "caret pad must mirror the source tab, got:\n{rendered:?}"
        );
        assert!(
            !rendered.contains(" ^^^"),
            "a space-padded run is the pre-lift drift, got:\n{rendered:?}"
        );
    }

    #[test]
    fn render_sizes_the_caret_run_in_chars_not_bytes() {
        // BUG FIX PIN. `éé` is 2 chars but 4 bytes. Pre-lift the width
        // was `span.end - span.start` (bytes) while the pad counted
        // chars, so this drew FOUR carets under a two-column symbol.
        let src = "(+ éé 1)";
        let start = src.find("éé").expect("fixture contains the symbol");
        let span = Span::new(start, start + "éé".len());
        let rendered = EvalError::unbound("éé", span).render(src);
        assert!(
            rendered.contains("   ^^\n") || rendered.ends_with("   ^^"),
            "two chars must draw exactly two carets, got:\n{rendered:?}"
        );
        assert!(
            !rendered.contains("^^^"),
            "a byte-sized run over-underlines multi-byte source, got:\n{rendered:?}"
        );
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
