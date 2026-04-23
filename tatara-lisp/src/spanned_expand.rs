//! Span-preserving macro expander.
//!
//! Mirror of `macro_expand::Expander` that operates on `Spanned` input and
//! produces `Spanned` output. Preserves source positions through macro
//! expansion so downstream evaluators can report errors at the exact
//! subform the user wrote (or, for macro-generated subtrees, at the
//! macro call site).
//!
//! This path is intentionally simpler than the plain `Expander`:
//!
//!   * No bytecode template compilation.
//!   * No expansion cache — args carry spans, so two calls with otherwise-
//!     identical args may differ by position, making the cache mostly
//!     useless here.
//!
//! The plain `Expander` on `Sexp` remains the fast path for the
//! `compile_typed` pipeline. This spanned path exists for `tatara-lisp-eval`
//! REPL + runtime evaluation where good error locations matter more than
//! throughput.

use std::collections::HashMap;

use crate::ast::Sexp;
use crate::error::{LispError, Result};
use crate::macro_expand::{MacroDef, Param};
use crate::span::Span;
use crate::spanned::{Spanned, SpannedForm};

/// Span-preserving macro expander.
#[derive(Clone, Default)]
pub struct SpannedExpander {
    macros: HashMap<String, MacroDef>,
}

impl SpannedExpander {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has(&self, name: &str) -> bool {
        self.macros.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.macros.len()
    }

    pub fn is_empty(&self) -> bool {
        self.macros.is_empty()
    }

    /// Expand a program. `defmacro`-family forms register and are consumed;
    /// remaining forms are expanded.
    pub fn expand_program(&mut self, forms: Vec<Spanned>) -> Result<Vec<Spanned>> {
        let mut out = Vec::new();
        for form in forms {
            if let Some(def) = spanned_macro_def_from(&form)? {
                self.macros.insert(def.name.clone(), def);
                continue;
            }
            out.push(self.expand(&form)?);
        }
        Ok(out)
    }

    /// Expand a single form. Top-level macro calls are rewritten; otherwise
    /// recurses into list children.
    pub fn expand(&self, form: &Spanned) -> Result<Spanned> {
        let SpannedForm::List(list) = &form.form else {
            return Ok(form.clone());
        };
        if let Some(head_name) = list.first().and_then(Spanned::as_symbol) {
            if let Some(def) = self.macros.get(head_name) {
                let expanded = self.apply(def, form.span, &list[1..])?;
                return self.expand(&expanded);
            }
        }
        let mut out_children: Vec<Spanned> = Vec::with_capacity(list.len());
        for child in list {
            out_children.push(self.expand(child)?);
        }
        Ok(Spanned::new(form.span, SpannedForm::List(out_children)))
    }

    /// Apply one macro definition at `call_span` to its spanned arguments.
    fn apply(&self, def: &MacroDef, call_span: Span, args: &[Spanned]) -> Result<Spanned> {
        let bindings = bind_spanned_args(&def.name, &def.params, args, call_span)?;
        let body = match &def.body {
            Sexp::Quasiquote(inner) => inner.as_ref(),
            other => other,
        };
        substitute_spanned(body, &bindings, call_span)
    }
}

/// Per-call binding from param name to spanned argument tree.
type Bindings = HashMap<String, Binding>;

#[derive(Clone, Debug)]
enum Binding {
    Single(Spanned),
    /// Rest parameter — holds a list of spanned arguments.
    Rest(Vec<Spanned>),
}

fn bind_spanned_args(
    macro_name: &str,
    params: &[Param],
    args: &[Spanned],
    _call_span: Span,
) -> Result<Bindings> {
    let mut bindings: Bindings = HashMap::new();
    let mut i = 0;
    for param in params {
        match param {
            Param::Required(name) => {
                let arg = args.get(i).cloned().ok_or_else(|| LispError::Compile {
                    form: format!("call to {macro_name}"),
                    message: format!("missing required arg: {name}"),
                })?;
                bindings.insert(name.clone(), Binding::Single(arg));
                i += 1;
            }
            Param::Rest(name) => {
                let rest: Vec<Spanned> = args.get(i..).unwrap_or(&[]).to_vec();
                bindings.insert(name.clone(), Binding::Rest(rest));
                i = args.len();
            }
        }
    }
    Ok(bindings)
}

/// Walk a plain-Sexp template body, substituting `,name` / `,@name` with
/// the spanned bindings and stamping literal template content with the
/// call-site span.
fn substitute_spanned(template: &Sexp, bindings: &Bindings, call_span: Span) -> Result<Spanned> {
    match template {
        Sexp::Unquote(inner) => {
            let sym = inner.as_symbol().ok_or_else(|| LispError::Compile {
                form: "unquote".into(),
                message: "only bound symbols may appear after `,`".into(),
            })?;
            match bindings.get(sym) {
                Some(Binding::Single(val)) => Ok(val.clone()),
                Some(Binding::Rest(_)) => Err(LispError::Compile {
                    form: format!(",{sym}"),
                    message: "cannot splice rest arg with `,` — use `,@`".into(),
                }),
                None => Err(LispError::Compile {
                    form: format!(",{sym}"),
                    message: "unbound".into(),
                }),
            }
        }
        Sexp::UnquoteSplice(_) => Err(LispError::Compile {
            form: "unquote-splice".into(),
            message: "`,@` may only appear inside a list".into(),
        }),
        Sexp::List(items) => {
            let mut out: Vec<Spanned> = Vec::with_capacity(items.len());
            for item in items {
                if let Sexp::UnquoteSplice(inner) = item {
                    let sym = inner.as_symbol().ok_or_else(|| LispError::Compile {
                        form: "unquote-splice".into(),
                        message: "only bound symbols may appear after `,@`".into(),
                    })?;
                    let binding = bindings.get(sym).ok_or_else(|| LispError::Compile {
                        form: format!(",@{sym}"),
                        message: "unbound".into(),
                    })?;
                    match binding {
                        Binding::Rest(items) => out.extend(items.iter().cloned()),
                        Binding::Single(sp) => match &sp.form {
                            SpannedForm::List(children) => out.extend(children.iter().cloned()),
                            SpannedForm::Nil => {}
                            _ => out.push(sp.clone()),
                        },
                    }
                } else {
                    out.push(substitute_spanned(item, bindings, call_span)?);
                }
            }
            Ok(Spanned::new(call_span, SpannedForm::List(out)))
        }
        Sexp::Quote(inner) => {
            let inner = substitute_spanned(inner, bindings, call_span)?;
            Ok(Spanned::new(call_span, SpannedForm::Quote(Box::new(inner))))
        }
        Sexp::Quasiquote(inner) => {
            let inner = substitute_spanned(inner, bindings, call_span)?;
            Ok(Spanned::new(
                call_span,
                SpannedForm::Quasiquote(Box::new(inner)),
            ))
        }
        Sexp::Nil => Ok(Spanned::new(call_span, SpannedForm::Nil)),
        Sexp::Atom(a) => Ok(Spanned::new(call_span, SpannedForm::Atom(a.clone()))),
    }
}

/// Recognize a spanned `(defmacro name (params) body)` / `defpoint-template`
/// / `defcheck` form and lower it to the plain `MacroDef` the registry
/// expects. Span information on the definition itself is not retained —
/// macros are keyed by name.
fn spanned_macro_def_from(form: &Spanned) -> Result<Option<MacroDef>> {
    let Some(list) = form.as_list() else {
        return Ok(None);
    };
    let Some(head) = list.first().and_then(Spanned::as_symbol) else {
        return Ok(None);
    };
    if !matches!(head, "defmacro" | "defpoint-template" | "defcheck") {
        return Ok(None);
    }
    if list.len() < 4 {
        return Err(LispError::Compile {
            form: head.to_string(),
            message: "(defmacro name (params) body) required".into(),
        });
    }
    let name = list[1]
        .as_symbol()
        .ok_or_else(|| LispError::Compile {
            form: head.to_string(),
            message: "expected name symbol".into(),
        })?
        .to_string();
    let param_list = list[2].as_list().ok_or_else(|| LispError::Compile {
        form: head.to_string(),
        message: "expected param list".into(),
    })?;
    let params = parse_params_spanned(param_list)?;
    let body = list[3].to_sexp();
    Ok(Some(MacroDef { name, params, body }))
}

fn parse_params_spanned(list: &[Spanned]) -> Result<Vec<Param>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < list.len() {
        let s = list[i].as_symbol().ok_or_else(|| LispError::Compile {
            form: "defmacro params".into(),
            message: "expected symbol".into(),
        })?;
        if s == "&rest" {
            let name = list
                .get(i + 1)
                .and_then(Spanned::as_symbol)
                .ok_or_else(|| LispError::Compile {
                    form: "defmacro params".into(),
                    message: "&rest needs a name".into(),
                })?;
            out.push(Param::Rest(name.to_string()));
            return Ok(out);
        }
        out.push(Param::Required(s.to_string()));
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{read, read_spanned};

    fn parse(src: &str) -> Sexp {
        read(src).unwrap().into_iter().next().unwrap()
    }

    #[test]
    fn identity_macro_preserves_arg_span() {
        let src = "(defmacro id (x) `,x) (id 42)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        let out = e.expand_program(forms).unwrap();
        assert_eq!(out.len(), 1);
        // The result is the literal 42 from the call site.
        assert_eq!(out[0].to_sexp(), Sexp::int(42));
        // Its span should point at the "42" in the source, not synthetic.
        assert!(!out[0].span.is_synthetic());
        let expected_start = src.find("42").unwrap();
        assert_eq!(out[0].span, Span::new(expected_start, expected_start + 2));
    }

    #[test]
    fn wrap_macro_substitution_preserves_each_arg_span() {
        let src = "(defmacro wrap (x) `(list ,x ,x)) (wrap hello)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        let out = e.expand_program(forms).unwrap();
        assert_eq!(out[0].to_sexp(), parse("(list hello hello)"));
        // The outer list span should cover the whole call site (wrap hello).
        let SpannedForm::List(children) = &out[0].form else {
            panic!()
        };
        // Literal `list` is stamped with the call-site span, not synthetic.
        let list_span = children[0].span;
        // Both substituted `hello` spans should be equal — they both come
        // from the same argument in the source.
        assert_eq!(children[1].span, children[2].span);
        assert_ne!(children[1].span, list_span);
        assert!(!children[1].span.is_synthetic());
    }

    #[test]
    fn rest_param_splice_preserves_argument_spans() {
        let src = "(defmacro call (f &rest args) `(,f ,@args)) (call foo a b c)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        let out = e.expand_program(forms).unwrap();
        assert_eq!(out[0].to_sexp(), parse("(foo a b c)"));
        let SpannedForm::List(children) = &out[0].form else {
            panic!()
        };
        // foo, a, b, c should all have non-synthetic spans covering their
        // positions in the source.
        for c in children {
            assert!(!c.span.is_synthetic(), "{:?}", c);
        }
    }

    #[test]
    fn nested_macro_expansion_preserves_original_arg_span() {
        let src = "(defmacro twice (x) `(list ,x ,x))
                   (defmacro quad (x) `(twice ,x))
                   (quad hey)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        let out = e.expand_program(forms).unwrap();
        assert_eq!(out[0].to_sexp(), parse("(list hey hey)"));
        let SpannedForm::List(children) = &out[0].form else {
            panic!()
        };
        // Both `hey` references should carry the argument's original span.
        assert!(!children[1].span.is_synthetic());
        assert_eq!(children[1].span, children[2].span);
    }

    #[test]
    fn non_macro_form_passes_through_with_original_spans() {
        let src = "(foo bar baz)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        let out = e.expand_program(forms).unwrap();
        assert_eq!(out[0].to_sexp(), parse("(foo bar baz)"));
        // Outer span covers whole source, children span their identifiers.
        assert_eq!(out[0].span, Span::new(0, src.len()));
    }

    #[test]
    fn unbound_unquote_errors() {
        let src = "(defmacro bad (x) `(list ,y)) (bad 1)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        assert!(e.expand_program(forms).is_err());
    }

    #[test]
    fn missing_required_arg_errors() {
        let src = "(defmacro need-two (a b) `(,a ,b)) (need-two 1)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        assert!(e.expand_program(forms).is_err());
    }

    #[test]
    fn empty_rest_splices_nothing() {
        let src = "(defmacro f (x &rest r) `(list ,x ,@r)) (f 1)";
        let forms = read_spanned(src).unwrap();
        let mut e = SpannedExpander::new();
        let out = e.expand_program(forms).unwrap();
        assert_eq!(out[0].to_sexp(), parse("(list 1)"));
    }

    #[test]
    fn agrees_with_plain_expander_on_output() {
        use crate::macro_expand::Expander;

        let src = "
            (defmacro wrap (x) `(list ,x ,x))
            (defmacro call (f &rest args) `(,f ,@args))
            (defmacro twice (x) `(list ,x ,x))
            (defmacro quad (x) `(twice ,x))
            (wrap hello)
            (call foo a b c)
            (quad hey)
            (outer (wrap deep))
        ";
        let plain_forms = read(src).unwrap();
        let spanned_forms = read_spanned(src).unwrap();

        let mut plain = Expander::new();
        let plain_out = plain.expand_program(plain_forms).unwrap();

        let mut spanned = SpannedExpander::new();
        let spanned_out = spanned.expand_program(spanned_forms).unwrap();

        assert_eq!(plain_out.len(), spanned_out.len());
        for (p, s) in plain_out.iter().zip(spanned_out.iter()) {
            assert_eq!(p, &s.to_sexp());
        }
    }
}
