//! Parallel spanned AST.
//!
//! Mirror of `ast::Sexp` where every node carries a `Span` back to its
//! source. Produced by `reader::read_spanned`, consumed by
//! `spanned_expand::SpannedExpander` and by downstream evaluators that want
//! to report error locations.
//!
//! The plain `Sexp` AST is unaffected. `Spanned::to_sexp` projects away the
//! span information when a consumer wants the canonical spanless form.

use crate::ast::{Atom, Sexp};
use crate::span::Span;

/// An S-expression node with source position.
#[derive(Clone, Debug, PartialEq)]
pub struct Spanned {
    pub span: Span,
    pub form: SpannedForm,
}

/// Same variants as `Sexp`, but children are `Spanned` so every subtree
/// carries its own position.
#[derive(Clone, Debug, PartialEq)]
pub enum SpannedForm {
    Nil,
    Atom(Atom),
    List(Vec<Spanned>),
    Quote(Box<Spanned>),
    Quasiquote(Box<Spanned>),
    Unquote(Box<Spanned>),
    UnquoteSplice(Box<Spanned>),
}

impl Spanned {
    pub fn new(span: Span, form: SpannedForm) -> Self {
        Self { span, form }
    }

    /// Synthetic nil — useful as a placeholder in macro expansion when no
    /// real span is available.
    pub fn synthetic_nil() -> Self {
        Self {
            span: Span::synthetic(),
            form: SpannedForm::Nil,
        }
    }

    /// Project away span information. Allocates a full `Sexp` tree.
    pub fn to_sexp(&self) -> Sexp {
        match &self.form {
            SpannedForm::Nil => Sexp::Nil,
            SpannedForm::Atom(a) => Sexp::Atom(a.clone()),
            SpannedForm::List(xs) => Sexp::List(xs.iter().map(Spanned::to_sexp).collect()),
            SpannedForm::Quote(inner) => Sexp::Quote(Box::new(inner.to_sexp())),
            SpannedForm::Quasiquote(inner) => Sexp::Quasiquote(Box::new(inner.to_sexp())),
            SpannedForm::Unquote(inner) => Sexp::Unquote(Box::new(inner.to_sexp())),
            SpannedForm::UnquoteSplice(inner) => Sexp::UnquoteSplice(Box::new(inner.to_sexp())),
        }
    }

    /// Lift a plain `Sexp` to a `Spanned` with every node marked synthetic.
    /// Useful when a macro template generates literal structure that has no
    /// user-source origin.
    pub fn from_sexp_synthetic(s: &Sexp) -> Self {
        Self::from_sexp_at(s, Span::synthetic())
    }

    /// Lift a plain `Sexp` to a `Spanned` with every node assigned the given
    /// span. Used by the expander to stamp macro-generated subtrees with
    /// the call-site span so errors point somewhere useful.
    pub fn from_sexp_at(s: &Sexp, span: Span) -> Self {
        let form = match s {
            Sexp::Nil => SpannedForm::Nil,
            Sexp::Atom(a) => SpannedForm::Atom(a.clone()),
            Sexp::List(xs) => {
                SpannedForm::List(xs.iter().map(|x| Spanned::from_sexp_at(x, span)).collect())
            }
            Sexp::Quote(inner) => SpannedForm::Quote(Box::new(Spanned::from_sexp_at(inner, span))),
            Sexp::Quasiquote(inner) => {
                SpannedForm::Quasiquote(Box::new(Spanned::from_sexp_at(inner, span)))
            }
            Sexp::Unquote(inner) => {
                SpannedForm::Unquote(Box::new(Spanned::from_sexp_at(inner, span)))
            }
            Sexp::UnquoteSplice(inner) => {
                SpannedForm::UnquoteSplice(Box::new(Spanned::from_sexp_at(inner, span)))
            }
        };
        Spanned { span, form }
    }

    // ── Convenience accessors mirroring Sexp ─────────────────────────

    pub fn is_list(&self) -> bool {
        matches!(self.form, SpannedForm::List(_))
    }
    pub fn as_list(&self) -> Option<&[Spanned]> {
        match &self.form {
            SpannedForm::List(xs) => Some(xs),
            _ => None,
        }
    }
    pub fn as_symbol(&self) -> Option<&str> {
        match &self.form {
            SpannedForm::Atom(Atom::Symbol(s)) => Some(s),
            _ => None,
        }
    }
    pub fn as_keyword(&self) -> Option<&str> {
        match &self.form {
            SpannedForm::Atom(Atom::Keyword(s)) => Some(s),
            _ => None,
        }
    }
    pub fn as_string(&self) -> Option<&str> {
        match &self.form {
            SpannedForm::Atom(Atom::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match &self.form {
            SpannedForm::Atom(Atom::Int(n)) => Some(*n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_sexp_roundtrip_preserves_structure() {
        let s = Spanned::new(
            Span::new(0, 10),
            SpannedForm::List(vec![
                Spanned::new(
                    Span::new(1, 4),
                    SpannedForm::Atom(Atom::Symbol("foo".into())),
                ),
                Spanned::new(Span::new(5, 6), SpannedForm::Atom(Atom::Int(42))),
            ]),
        );
        let plain = s.to_sexp();
        assert_eq!(plain, Sexp::List(vec![Sexp::symbol("foo"), Sexp::int(42)]));
    }

    #[test]
    fn from_sexp_synthetic_marks_all_nodes() {
        let s = Sexp::List(vec![Sexp::symbol("a"), Sexp::List(vec![Sexp::int(1)])]);
        let lifted = Spanned::from_sexp_synthetic(&s);
        assert!(lifted.span.is_synthetic());
        let SpannedForm::List(children) = &lifted.form else {
            panic!("expected list")
        };
        assert!(children[0].span.is_synthetic());
        let SpannedForm::List(inner) = &children[1].form else {
            panic!("expected inner list")
        };
        assert!(inner[0].span.is_synthetic());
    }

    #[test]
    fn from_sexp_at_stamps_span_on_every_node() {
        let s = Sexp::List(vec![Sexp::symbol("a"), Sexp::int(1)]);
        let sp = Span::new(10, 20);
        let lifted = Spanned::from_sexp_at(&s, sp);
        assert_eq!(lifted.span, sp);
        let SpannedForm::List(children) = &lifted.form else {
            panic!()
        };
        assert_eq!(children[0].span, sp);
        assert_eq!(children[1].span, sp);
    }
}
