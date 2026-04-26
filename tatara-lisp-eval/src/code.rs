//! Bidirectional `Spanned` ↔ `Value` conversion.
//!
//! At macro-expansion time, the macro body is a regular Lisp program
//! that runs in the live `Interpreter`. To make that work, the macro's
//! source-form arguments must reach the body as runtime Values
//! (otherwise primitives like `car`, `map`, `cons` couldn't manipulate
//! them), and the body's result Value must be converted back to a
//! `Spanned` tree for the eval loop to consume.
//!
//! The encoding is structural and lossless for ordinary forms:
//!
//! ```text
//!   Spanned::Atom(Symbol s)   ↔  Value::Symbol s
//!   Spanned::Atom(Keyword s)  ↔  Value::Keyword s
//!   Spanned::Atom(Str s)      ↔  Value::Str s
//!   Spanned::Atom(Int n)      ↔  Value::Int n
//!   Spanned::Atom(Float n)    ↔  Value::Float n
//!   Spanned::Atom(Bool b)     ↔  Value::Bool b
//!   Spanned::Nil              ↔  Value::Nil
//!   Spanned::List xs          ↔  Value::List (xs lowered)
//!   Spanned::Quote(x)         ↔  Value::List ('quote, lower(x))
//!   Spanned::Quasiquote(x)    ↔  Value::List ('quasiquote, lower(x))
//!   Spanned::Unquote(x)       ↔  Value::List ('unquote, lower(x))
//!   Spanned::UnquoteSplice(x) ↔  Value::List ('unquote-splice, lower(x))
//! ```
//!
//! The quote/quasiquote/etc forms become explicit `(quote x)` lists
//! so macros can inspect, manipulate, and produce them uniformly. The
//! reverse direction recognizes the head symbol and lifts back to the
//! corresponding `SpannedForm` variant.
//!
//! Source positions: lowering Spanned → Value loses span info (Values
//! don't carry spans). Lifting Value → Spanned stamps every produced
//! node with the call-site span — exactly what we want for macro
//! output, where the user "wrote it" at the macro call site.

use std::sync::Arc;

use tatara_lisp::{Atom, Span, Spanned, SpannedForm};

use crate::value::Value;

/// Lower a Spanned tree to a Value tree for macro-time manipulation.
/// Quote / quasiquote / unquote / unquote-splice are encoded as
/// 2-element lists with the corresponding head symbol so macros can
/// inspect them as ordinary data.
pub fn spanned_to_value(s: &Spanned) -> Value {
    match &s.form {
        SpannedForm::Nil => Value::Nil,
        SpannedForm::Atom(a) => atom_to_value(a),
        SpannedForm::List(xs) => Value::list(xs.iter().map(spanned_to_value)),
        SpannedForm::Quote(inner) => {
            wrap_with_head_symbol("quote", spanned_to_value(inner))
        }
        SpannedForm::Quasiquote(inner) => {
            wrap_with_head_symbol("quasiquote", spanned_to_value(inner))
        }
        SpannedForm::Unquote(inner) => {
            wrap_with_head_symbol("unquote", spanned_to_value(inner))
        }
        SpannedForm::UnquoteSplice(inner) => {
            wrap_with_head_symbol("unquote-splice", spanned_to_value(inner))
        }
    }
}

fn atom_to_value(a: &Atom) -> Value {
    match a {
        Atom::Symbol(s) => Value::Symbol(Arc::from(s.as_str())),
        Atom::Keyword(s) => Value::Keyword(Arc::from(s.as_str())),
        Atom::Str(s) => Value::Str(Arc::from(s.as_str())),
        Atom::Int(n) => Value::Int(*n),
        Atom::Float(n) => Value::Float(*n),
        Atom::Bool(b) => Value::Bool(*b),
    }
}

fn wrap_with_head_symbol(head: &'static str, inner: Value) -> Value {
    Value::list(vec![Value::Symbol(Arc::from(head)), inner])
}

/// Lift a Value back into a Spanned tree, stamping every node with
/// `span`. Recognizes `(quote x)` / `(quasiquote x)` / `(unquote x)` /
/// `(unquote-splice x)` head-symbol shape and produces the matching
/// `SpannedForm` variant — round-trip with `spanned_to_value`.
///
/// Errors:
/// * `Value::Closure` and `Value::NativeFn` cannot be lifted — they're
///   runtime objects with no source-form representation. Callers
///   should never produce these from a macro body; we report a
///   diagnostic if they do.
pub fn value_to_spanned(v: &Value, span: Span) -> Result<Spanned, String> {
    match v {
        Value::Nil => Ok(Spanned::new(span, SpannedForm::Nil)),
        Value::Bool(b) => Ok(Spanned::new(span, SpannedForm::Atom(Atom::Bool(*b)))),
        Value::Int(n) => Ok(Spanned::new(span, SpannedForm::Atom(Atom::Int(*n)))),
        Value::Float(n) => Ok(Spanned::new(span, SpannedForm::Atom(Atom::Float(*n)))),
        Value::Str(s) => Ok(Spanned::new(span, SpannedForm::Atom(Atom::Str(s.to_string())))),
        Value::Symbol(s) => Ok(Spanned::new(
            span,
            SpannedForm::Atom(Atom::Symbol(s.to_string())),
        )),
        Value::Keyword(s) => Ok(Spanned::new(
            span,
            SpannedForm::Atom(Atom::Keyword(s.to_string())),
        )),
        Value::List(xs) => {
            // Detect head-symbol-wrapped forms: (quote x) / (quasiquote x) /
            // (unquote x) / (unquote-splice x).
            if xs.len() == 2 {
                if let Value::Symbol(head) = &xs[0] {
                    let inner_spanned = value_to_spanned(&xs[1], span)?;
                    let lifted = match head.as_ref() {
                        "quote" => Some(SpannedForm::Quote(Box::new(inner_spanned.clone()))),
                        "quasiquote" => {
                            Some(SpannedForm::Quasiquote(Box::new(inner_spanned.clone())))
                        }
                        "unquote" => Some(SpannedForm::Unquote(Box::new(inner_spanned.clone()))),
                        "unquote-splice" => {
                            Some(SpannedForm::UnquoteSplice(Box::new(inner_spanned.clone())))
                        }
                        _ => None,
                    };
                    if let Some(form) = lifted {
                        return Ok(Spanned::new(span, form));
                    }
                }
            }
            let children: Result<Vec<Spanned>, String> =
                xs.iter().map(|x| value_to_spanned(x, span)).collect();
            Ok(Spanned::new(span, SpannedForm::List(children?)))
        }
        Value::Sexp(sexp, sp) => Ok(Spanned::from_sexp_at(sexp, *sp)),
        Value::Error(_) => Err(
            "macro body returned an Error value — errors cannot be \
             converted to source forms (use `throw` to raise instead)"
                .to_string(),
        ),
        Value::Closure(_) => Err(
            "macro body returned a closure — closures cannot be \
             converted to source forms (did you mean to call the closure?)"
                .to_string(),
        ),
        Value::NativeFn(_) => Err(
            "macro body returned a native function — native fns \
             cannot be converted to source forms"
                .to_string(),
        ),
        Value::Foreign(_) => Err(
            "macro body returned a foreign Value — foreign values \
             cannot be converted to source forms"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tatara_lisp::read_spanned;

    fn parse(src: &str) -> Spanned {
        read_spanned(src).unwrap().pop().unwrap()
    }

    #[test]
    fn round_trip_atom() {
        let s = parse("42");
        let v = spanned_to_value(&s);
        assert!(matches!(v, Value::Int(42)));
        let s2 = value_to_spanned(&v, s.span).unwrap();
        assert_eq!(s2.form, s.form);
    }

    #[test]
    fn round_trip_list() {
        let s = parse("(a b c)");
        let v = spanned_to_value(&s);
        match &v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[0], Value::Symbol(s) if &**s == "a"));
            }
            _ => panic!(),
        }
        let s2 = value_to_spanned(&v, s.span).unwrap();
        assert_eq!(s2.to_sexp(), s.to_sexp());
    }

    #[test]
    fn round_trip_quote() {
        let s = parse("'foo");
        let v = spanned_to_value(&s);
        // 'foo lowers to (quote foo)
        match &v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 2);
                assert!(matches!(&xs[0], Value::Symbol(h) if &**h == "quote"));
                assert!(matches!(&xs[1], Value::Symbol(s) if &**s == "foo"));
            }
            _ => panic!(),
        }
        // Round-trip: should reproduce SpannedForm::Quote(...)
        let s2 = value_to_spanned(&v, s.span).unwrap();
        assert!(matches!(s2.form, SpannedForm::Quote(_)));
    }

    #[test]
    fn round_trip_quasiquote_with_unquote() {
        let s = parse("`(a ,b c)");
        let v = spanned_to_value(&s);
        let s2 = value_to_spanned(&v, s.span).unwrap();
        // The Sexp form should match precisely.
        assert_eq!(s2.to_sexp(), s.to_sexp());
    }

    #[test]
    fn round_trip_nested_unquote_splice() {
        let s = parse("`(a ,@xs c)");
        let v = spanned_to_value(&s);
        let s2 = value_to_spanned(&v, s.span).unwrap();
        assert_eq!(s2.to_sexp(), s.to_sexp());
    }

    #[test]
    fn round_trip_string() {
        let s = parse("\"hello\"");
        let v = spanned_to_value(&s);
        assert!(matches!(&v, Value::Str(s) if &**s == "hello"));
        let s2 = value_to_spanned(&v, s.span).unwrap();
        assert_eq!(s2.to_sexp(), s.to_sexp());
    }

    #[test]
    fn closure_value_cannot_lift() {
        use crate::env::Env;
        use crate::value::Closure;
        let c = Closure {
            params: vec![],
            rest: None,
            body: vec![],
            captured_env: Env::new(),
            source: Span::synthetic(),
        };
        let v = Value::Closure(Arc::new(c));
        let r = value_to_spanned(&v, Span::synthetic());
        assert!(r.is_err());
    }
}
