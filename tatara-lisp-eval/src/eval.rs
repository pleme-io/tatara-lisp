//! Core evaluator.
//!
//! Phase 2.2 scaffold: `Interpreter<H>` shape, constructor, and a minimal
//! `eval_spanned` that handles only literal atoms. Special forms, function
//! application, and closure invocation land in Phase 2.3.

use std::sync::Arc;

use tatara_lisp::{Atom, Spanned, SpannedForm};

use crate::env::Env;
use crate::error::{EvalError, Result};
use crate::ffi::FnRegistry;
use crate::value::Value;

/// An embedded tatara-lisp evaluator, parameterized over the host context
/// `H` that registered functions read/write.
pub struct Interpreter<H> {
    // Populated by register_fn in Phase 2.4; used by eval dispatch in
    // Phase 2.3+.
    #[allow(dead_code)]
    pub(crate) registry: FnRegistry<H>,
    pub(crate) globals: Env,
}

impl<H> Interpreter<H> {
    pub fn new() -> Self {
        Self {
            registry: FnRegistry::new(),
            globals: Env::new(),
        }
    }

    /// Evaluate a single already-read form in the given host context.
    ///
    /// Phase 2.2: handles only literal atoms (int, float, string, bool,
    /// keyword, nil). Symbols look up in the global env. Lists and special
    /// forms return `EvalError::NotImplemented`. Phase 2.3 fills this in.
    pub fn eval_spanned(&mut self, form: &Spanned, _host: &mut H) -> Result<Value> {
        match &form.form {
            SpannedForm::Nil => Ok(Value::Nil),
            SpannedForm::Atom(a) => Ok(atom_to_value(a)),
            SpannedForm::List(_) => Err(EvalError::NotImplemented("list / function application")),
            SpannedForm::Quote(_) => Err(EvalError::NotImplemented("quote")),
            SpannedForm::Quasiquote(_) => Err(EvalError::NotImplemented("quasiquote")),
            SpannedForm::Unquote(_) | SpannedForm::UnquoteSplice(_) => Err(EvalError::bad_form(
                "unquote",
                "unquote outside of quasiquote",
                form.span,
            )),
        }
    }

    /// Look up a symbol in the global env.
    pub fn lookup_global(&self, name: &str) -> Option<&Value> {
        self.globals.lookup(name)
    }

    /// Bind a value in the global env.
    pub fn define_global(&mut self, name: impl Into<Arc<str>>, value: Value) {
        self.globals.define(name, value);
    }
}

impl<H> Default for Interpreter<H> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;
    use tatara_lisp::read_spanned;

    struct NoHost;

    #[test]
    fn literal_int_evaluates_to_itself() {
        let forms = read_spanned("42").unwrap();
        let mut i: Interpreter<NoHost> = Interpreter::new();
        let mut host = NoHost;
        let v = i.eval_spanned(&forms[0], &mut host).unwrap();
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn literal_string_evaluates_to_itself() {
        let forms = read_spanned("\"hi\"").unwrap();
        let mut i: Interpreter<NoHost> = Interpreter::new();
        let mut host = NoHost;
        let v = i.eval_spanned(&forms[0], &mut host).unwrap();
        match v {
            Value::Str(s) => assert_eq!(&*s, "hi"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn literal_bool_and_keyword() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        let mut host = NoHost;

        let f = read_spanned("#t").unwrap();
        assert!(matches!(
            i.eval_spanned(&f[0], &mut host).unwrap(),
            Value::Bool(true)
        ));

        let f = read_spanned("#f").unwrap();
        assert!(matches!(
            i.eval_spanned(&f[0], &mut host).unwrap(),
            Value::Bool(false)
        ));

        let f = read_spanned(":tag").unwrap();
        match i.eval_spanned(&f[0], &mut host).unwrap() {
            Value::Keyword(k) => assert_eq!(&*k, "tag"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lists_return_not_implemented_until_phase_2_3() {
        let forms = read_spanned("(+ 1 2)").unwrap();
        let mut i: Interpreter<NoHost> = Interpreter::new();
        let mut host = NoHost;
        let err = i.eval_spanned(&forms[0], &mut host).unwrap_err();
        assert!(matches!(err, EvalError::NotImplemented(_)));
    }

    #[test]
    fn globals_round_trip() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        i.define_global("x", Value::Int(99));
        assert!(matches!(i.lookup_global("x"), Some(Value::Int(99))));
        assert!(i.lookup_global("missing").is_none());
    }
}
