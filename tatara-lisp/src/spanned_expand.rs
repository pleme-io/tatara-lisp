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

    /// Look up a registered macro by name. `None` if unknown.
    pub fn get_macro(&self, name: &str) -> Option<&MacroDef> {
        self.macros.get(name)
    }

    /// All registered macro names. Order is unspecified.
    pub fn macro_names(&self) -> impl Iterator<Item = &str> {
        self.macros.keys().map(|s| s.as_str())
    }

    /// Recognize `defmacro` / `defpoint-template` / `defcheck` and register
    /// the definition. Returns `true` if `form` was a macro definition
    /// (and was consumed), `false` if it was an ordinary form. Used by
    /// embedders that interleave registration with evaluation form-by-form
    /// (e.g. `tatara-lisp-eval`'s REPL).
    pub fn try_register_macro(&mut self, form: &Spanned) -> Result<bool> {
        if let Some(def) = spanned_macro_def_from(form)? {
            self.macros.insert(def.name.clone(), def);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Expand a program. `defmacro`-family forms register and are consumed;
    /// remaining forms are expanded.
    pub fn expand_program(&mut self, forms: Vec<Spanned>) -> Result<Vec<Spanned>> {
        let mut out = Vec::new();
        for form in forms {
            if self.try_register_macro(&form)? {
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
///
/// Inside `,expr`, the expression is evaluated at expansion time against
/// the macro's parameter bindings — a tiny built-in template-time
/// evaluator handles bare symbols, `car`/`cdr`/`cons`/`list`/`null?`/
/// `pair?`/`length`/`if`/`quote`, and literal atoms. This is enough
/// expressive power for the `->` / `->>` / threading macros and other
/// recursive macro definitions that need to dispatch on rest-arg shape.
fn substitute_spanned(template: &Sexp, bindings: &Bindings, call_span: Span) -> Result<Spanned> {
    match template {
        Sexp::Unquote(inner) => template_eval(inner, bindings, call_span),
        Sexp::UnquoteSplice(_) => Err(LispError::Compile {
            form: "unquote-splice".into(),
            message: "`,@` may only appear inside a list".into(),
        }),
        Sexp::List(items) => {
            let mut out: Vec<Spanned> = Vec::with_capacity(items.len());
            for item in items {
                if let Sexp::UnquoteSplice(inner) = item {
                    let evaluated = template_eval(inner, bindings, call_span)?;
                    splice_into(&evaluated, &mut out);
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

/// Splice `evaluated` into the surrounding list builder. List values
/// flatten in; nil disappears; everything else is pushed as a single item.
fn splice_into(evaluated: &Spanned, out: &mut Vec<Spanned>) {
    match &evaluated.form {
        SpannedForm::List(children) => out.extend(children.iter().cloned()),
        SpannedForm::Nil => {}
        _ => out.push(evaluated.clone()),
    }
}

/// Template-time evaluator. Lives inside `,expr` and walks a Sexp
/// template expression, substituting bindings and computing a result
/// Spanned tree. Intentionally bounded — supports the operations
/// needed for self-recursive macros that pattern-match on rest args.
///
/// Supports:
///
/// * Bare symbols → look up in `bindings` (Single binding returns its
///   Spanned; Rest returns a Spanned::List of the rest items).
/// * Atoms (Int / Float / Str / Bool / Keyword) → wrapped with
///   `call_span`.
/// * `(quote x)` → x lifted to Spanned without evaluation.
/// * `(car x)`, `(cdr x)`, `(cons h t)`, `(list ...)` — list ops on
///   evaluated children.
/// * `(null? x)`, `(pair? x)`, `(list? x)` — predicates → `Bool` Spanned.
/// * `(length x)` → integer Spanned.
/// * `(if c t e)` — picks branch by truthiness of the evaluated cond.
///
/// Anything else is rejected with a clear error.
fn template_eval(expr: &Sexp, bindings: &Bindings, call_span: Span) -> Result<Spanned> {
    match expr {
        Sexp::Atom(crate::ast::Atom::Symbol(name)) => {
            // Bare symbol — look up in bindings.
            match bindings.get(name) {
                Some(Binding::Single(val)) => Ok(val.clone()),
                Some(Binding::Rest(items)) => {
                    Ok(Spanned::new(call_span, SpannedForm::List(items.clone())))
                }
                None => Err(LispError::Compile {
                    form: format!(",{name}"),
                    message: "unbound in macro template".into(),
                }),
            }
        }
        Sexp::Atom(a) => Ok(Spanned::new(call_span, SpannedForm::Atom(a.clone()))),
        Sexp::Nil => Ok(Spanned::new(call_span, SpannedForm::Nil)),
        Sexp::Quote(inner) => Ok(Spanned::from_sexp_at(inner, call_span)),
        // `\`expr` at template-eval time MEANS "produce the substituted
        // form of expr" — i.e., re-enter substitution. This is how a
        // recursive macro template reaches its else-branch, e.g.
        // `(-> ,x ,(if (null? steps) `,result `(-> ,inner ,@rest)))`.
        Sexp::Quasiquote(inner) => substitute_spanned(inner, bindings, call_span),
        // `,expr` inside template_eval just unwraps one level — it
        // identifies an expression to evaluate, which is exactly what
        // template_eval is doing anyway.
        Sexp::Unquote(inner) => template_eval(inner, bindings, call_span),
        Sexp::UnquoteSplice(_) => Err(LispError::Compile {
            form: "template-eval".into(),
            message: "`,@` only valid directly inside a list".into(),
        }),
        Sexp::List(items) => {
            if items.is_empty() {
                return Ok(Spanned::new(call_span, SpannedForm::List(Vec::new())));
            }
            let head = items[0].as_symbol().ok_or_else(|| LispError::Compile {
                form: "template-eval".into(),
                message: "first element of a template-time list must be a symbol".into(),
            })?;
            match head {
                "quote" => {
                    let arg = items.get(1).ok_or_else(|| LispError::Compile {
                        form: "quote".into(),
                        message: "expected one arg".into(),
                    })?;
                    Ok(Spanned::from_sexp_at(arg, call_span))
                }
                "car" => {
                    let xs = template_eval_list(&items[1..], 1, "car", bindings, call_span)?;
                    let inner = template_eval(&xs[0].1, bindings, call_span)?;
                    let list = require_spanned_list(&inner, "car")?;
                    if list.is_empty() {
                        return Err(LispError::Compile {
                            form: "car".into(),
                            message: "car of empty list".into(),
                        });
                    }
                    Ok(list[0].clone())
                }
                "cdr" => {
                    let xs = template_eval_list(&items[1..], 1, "cdr", bindings, call_span)?;
                    let inner = template_eval(&xs[0].1, bindings, call_span)?;
                    let list = require_spanned_list(&inner, "cdr")?;
                    if list.is_empty() {
                        return Err(LispError::Compile {
                            form: "cdr".into(),
                            message: "cdr of empty list".into(),
                        });
                    }
                    Ok(Spanned::new(
                        call_span,
                        SpannedForm::List(list[1..].to_vec()),
                    ))
                }
                "cons" => {
                    let xs = template_eval_list(&items[1..], 2, "cons", bindings, call_span)?;
                    let h = template_eval(&xs[0].1, bindings, call_span)?;
                    let t = template_eval(&xs[1].1, bindings, call_span)?;
                    let mut out = vec![h];
                    match t.form {
                        SpannedForm::List(children) => out.extend(children),
                        SpannedForm::Nil => {}
                        _ => out.push(t),
                    }
                    Ok(Spanned::new(call_span, SpannedForm::List(out)))
                }
                "list" => {
                    let mut out: Vec<Spanned> = Vec::with_capacity(items.len() - 1);
                    for child in &items[1..] {
                        out.push(template_eval(child, bindings, call_span)?);
                    }
                    Ok(Spanned::new(call_span, SpannedForm::List(out)))
                }
                "null?" => {
                    let xs = template_eval_list(&items[1..], 1, "null?", bindings, call_span)?;
                    let v = template_eval(&xs[0].1, bindings, call_span)?;
                    let is_null = matches!(&v.form, SpannedForm::Nil)
                        || matches!(&v.form, SpannedForm::List(c) if c.is_empty());
                    Ok(Spanned::new(
                        call_span,
                        SpannedForm::Atom(crate::ast::Atom::Bool(is_null)),
                    ))
                }
                "pair?" => {
                    let xs = template_eval_list(&items[1..], 1, "pair?", bindings, call_span)?;
                    let v = template_eval(&xs[0].1, bindings, call_span)?;
                    let ok = matches!(&v.form, SpannedForm::List(c) if !c.is_empty());
                    Ok(Spanned::new(
                        call_span,
                        SpannedForm::Atom(crate::ast::Atom::Bool(ok)),
                    ))
                }
                "list?" => {
                    let xs = template_eval_list(&items[1..], 1, "list?", bindings, call_span)?;
                    let v = template_eval(&xs[0].1, bindings, call_span)?;
                    let ok = matches!(&v.form, SpannedForm::List(_) | SpannedForm::Nil);
                    Ok(Spanned::new(
                        call_span,
                        SpannedForm::Atom(crate::ast::Atom::Bool(ok)),
                    ))
                }
                "length" => {
                    let xs = template_eval_list(&items[1..], 1, "length", bindings, call_span)?;
                    let v = template_eval(&xs[0].1, bindings, call_span)?;
                    let n = match &v.form {
                        SpannedForm::Nil => 0,
                        SpannedForm::List(c) => c.len() as i64,
                        _ => {
                            return Err(LispError::Compile {
                                form: "length".into(),
                                message: "expected a list".into(),
                            })
                        }
                    };
                    Ok(Spanned::new(
                        call_span,
                        SpannedForm::Atom(crate::ast::Atom::Int(n)),
                    ))
                }
                "if" => {
                    if items.len() != 4 {
                        return Err(LispError::Compile {
                            form: "if".into(),
                            message: "expected (if cond then else)".into(),
                        });
                    }
                    let c = template_eval(&items[1], bindings, call_span)?;
                    let truthy = !matches!(
                        &c.form,
                        SpannedForm::Nil | SpannedForm::Atom(crate::ast::Atom::Bool(false))
                    );
                    if truthy {
                        template_eval(&items[2], bindings, call_span)
                    } else {
                        template_eval(&items[3], bindings, call_span)
                    }
                }
                other => Err(LispError::Compile {
                    form: other.into(),
                    message: "operation not supported in macro template `,expr`. Supported: \
                         quote, car, cdr, cons, list, null?, pair?, list?, length, if"
                        .into(),
                }),
            }
        }
    }
}

/// Helper: collect indexed (i, &Sexp) for a template-eval call's args,
/// checking arity. Lets the call sites get clear error messages.
fn template_eval_list<'a>(
    args: &'a [Sexp],
    expected: usize,
    fn_name: &'static str,
    _bindings: &Bindings,
    _call_span: Span,
) -> Result<Vec<(usize, &'a Sexp)>> {
    if args.len() != expected {
        return Err(LispError::Compile {
            form: fn_name.into(),
            message: format!("expected {expected} args, got {}", args.len()),
        });
    }
    Ok(args.iter().enumerate().collect())
}

fn require_spanned_list<'a>(s: &'a Spanned, fn_name: &'static str) -> Result<&'a [Spanned]> {
    match &s.form {
        SpannedForm::List(c) => Ok(c.as_slice()),
        SpannedForm::Nil => Ok(&[]),
        _ => Err(LispError::Compile {
            form: fn_name.into(),
            message: "expected a list".into(),
        }),
    }
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
