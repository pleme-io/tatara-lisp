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
//!
//! ── expander unification (phase 2 step 5c) ────────────────────────────
//!
//! Step 5b unified the READER — one tokenizer, one `Atom::from_lexeme`,
//! two projections. Step 5c unified the DEFINITION and BINDING halves of
//! the expander. What used to be four duplicate bodies in this file is
//! now:
//!
//!   * `parse_params_spanned` — **deleted**. Lambda lists are pure syntax
//!     and a `MacroDef` retains no spans, so there is one parser.
//!   * `spanned_macro_def_from` — a head-keyword pre-check plus a call to
//!     `macro_expand::macro_def_from`. One recognizer, one error taxonomy.
//!   * `bind_spanned_args` — a zip of the shared
//!     `MacroParams::bind_carrier` against `names()`. One binding loop,
//!     generic over the value carrier via `MacroArgCarrier`.
//!   * `substitute_spanned` + `template_eval` — still this file's own, and
//!     deliberately so; see the residue note below.
//!
//! Adopting A's `MacroParams` gave this path `&optional` (with per-param
//! defaults) and the too-many-args rejection it never had, with A's
//! semantics rather than a second implementation written in passing —
//! which is exactly the split-brain the pre-5c note refused to create.
//!
//! ── `pending-template-eval-unification` (residue) ─────────────────────
//!
//! `template_eval` — the `car`/`cdr`/`cons`/`list`/`null?`/`pair?`/
//! `list?`/`length`/`if` metalanguage that lives inside `,expr` — is
//! still one-sided: the plain `Expander` does NOT have it, and rejects
//! any `,expr` whose target is not a bare bound symbol.
//!
//! Measured 2026-07-30, so the next attempt does not re-derive it: this
//! is NOT a copy, it is a semantic ADDITION to the canonical path. The
//! plain expander's default strategy compiles each template to a linear
//! bytecode (`TemplateOp::{Literal, Subst(idx), Splice(idx), BeginList,
//! EndList}`) in which an unquote is an INDEX into the bound-arg vec —
//! `compile_node` resolves `,x` through `unquote_target_symbol` +
//! `resolve_param_index`, both of which structurally require a symbol.
//! A `,(car x)` has no index to compile to. Hoisting `template_eval`
//! therefore means a new `TemplateOp` variant carrying an unevaluated
//! `Sexp` plus an evaluator run at apply time, and it widens the accepted
//! language of every existing consumer of the plain path. That is a
//! design change to A's canonical semantics, not a consolidation of two
//! copies of one semantics — so it is deliberately NOT bundled into the
//! step that removes duplicates.
//!
//! The remaining duplication is `substitute_spanned`'s walk, which mirrors
//! `macro_expand::substitute`. It cannot collapse before `template_eval`
//! does: the two walks differ precisely at the unquote arm, where this one
//! calls `template_eval` and the plain one looks up a name.

use std::collections::HashMap;

use crate::ast::Sexp;
use crate::error::{LispError, MacroDefHead, Result};
use crate::macro_expand::{macro_def_from, MacroArgCarrier, MacroDef};
use crate::span::Span;
use crate::spanned::{Spanned, SpannedForm};

impl MacroArgCarrier for Spanned {
    /// The macro CALL SITE. A value the call never supplied — an unfilled
    /// `&optional` slot's default form, or the synthesized `&rest` list —
    /// has no source position of its own, so it wears the span of the call
    /// that caused it to exist. That is the position an error about such a
    /// value should point at.
    type Site = Span;

    fn lift_default(default: &Sexp, site: Span) -> Self {
        Spanned::from_sexp_at(default, site)
    }

    fn collect_rest(items: Vec<Self>, site: Span) -> Self {
        Spanned::new(site, SpannedForm::List(items))
    }
}

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
        substitute_spanned(def.template_body(), &bindings, call_span)
    }
}

/// Per-call binding from param name to spanned argument tree.
///
/// A `&rest` binding is an ordinary `SpannedForm::List` value rather than a
/// distinguished variant: `template_eval` already projected the old
/// `Binding::Rest` to exactly that list, and `splice_into` already flattened
/// any list value. Collapsing the enum is what lets the shared
/// [`MacroParams::bind_carrier`](crate::macro_expand::MacroParams::bind_carrier)
/// produce these bindings directly.
type Bindings = HashMap<String, Spanned>;

/// Bind a macro call's spanned args through the ONE shared positional binder
/// on [`MacroParams`](crate::macro_expand::MacroParams), then zip the result
/// against `names()` into the name-keyed map `substitute_spanned` /
/// `template_eval` look substitutions up in — the span-carrying mirror of
/// `macro_expand::bind_args`.
///
/// The lambda-list semantics (required run, `&optional` run with per-param
/// defaults, at-most-one `&rest`, too-few and too-many arity rejections) are
/// no longer restated here; this path and the plain `Sexp` path cannot
/// disagree about them because they run the same loop.
fn bind_spanned_args(
    macro_name: &str,
    params: &crate::macro_expand::MacroParams,
    args: &[Spanned],
    call_span: Span,
) -> Result<Bindings> {
    let vals = params.bind_carrier(macro_name, args, call_span)?;
    Ok(params
        .names()
        .into_iter()
        .map(String::from)
        .zip(vals)
        .collect())
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
///
/// The recognition itself is delegated to
/// [`macro_expand::macro_def_from`](crate::macro_expand::macro_def_from), so
/// the `defmacro`-head set, the lambda-list grammar (including `&optional`
/// with defaults) and the definition-site error taxonomy are shared with the
/// plain expander rather than restated here. Since a `MacroDef` retains no
/// spans, lowering the form loses nothing.
///
/// The head-keyword pre-check is not redundant: `try_register_macro` runs on
/// EVERY top-level form, and it is what keeps the `to_sexp()` lowering off
/// the path of ordinary (non-definition) forms.
fn spanned_macro_def_from(form: &Spanned) -> Result<Option<MacroDef>> {
    let Some(list) = form.as_list() else {
        return Ok(None);
    };
    let Some(head) = list.first().and_then(Spanned::as_symbol) else {
        return Ok(None);
    };
    if MacroDefHead::from_keyword(head).is_none() {
        return Ok(None);
    }
    macro_def_from(&form.to_sexp())
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
            // Bare symbol — look up in bindings. A `&rest` binding is
            // already a `SpannedForm::List` stamped at the call site by
            // `MacroArgCarrier::collect_rest`, so there is no rest-specific
            // arm to take here.
            match bindings.get(name) {
                Some(val) => Ok(val.clone()),
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

    /// `&optional` with a declared default reaches this path at all — it
    /// could not before step 5c, because `parse_params_spanned` knew only
    /// required and `&rest` and would have rejected `&optional` as a param
    /// NAME. Pins both arms of `OptionalParam::resolved_default` through
    /// `MacroArgCarrier::lift_default`: supplied wins, absent falls back.
    #[test]
    fn optional_param_default_agrees_with_plain_expander() {
        use crate::macro_expand::Expander;

        let src = "
            (defmacro greet (name &optional (greeting \"hi\") punct)
              `(list ,greeting ,name ,punct))
            (greet bob)
            (greet bob \"yo\")
            (greet bob \"yo\" bang)
        ";
        let plain_out = Expander::new().expand_program(read(src).unwrap()).unwrap();
        let spanned_out = SpannedExpander::new()
            .expand_program(read_spanned(src).unwrap())
            .unwrap();

        assert_eq!(plain_out.len(), 3);
        assert_eq!(plain_out.len(), spanned_out.len());
        for (p, s) in plain_out.iter().zip(spanned_out.iter()) {
            assert_eq!(p, &s.to_sexp());
        }
        // The declared default fills the absent slot; the bare optional
        // falls to the `Sexp::Nil` floor (which is NOT the empty list — an
        // authored `()` reads as `Sexp::List(vec![])`).
        let listed = |trailing: Sexp| {
            Sexp::List(vec![
                Sexp::symbol("list"),
                Sexp::string("hi"),
                Sexp::symbol("bob"),
                trailing,
            ])
        };
        assert_eq!(plain_out[0], listed(Sexp::Nil));
        // A supplied arg wins over the declared default.
        assert_eq!(
            plain_out[1],
            Sexp::List(vec![
                Sexp::symbol("list"),
                Sexp::string("yo"),
                Sexp::symbol("bob"),
                Sexp::Nil,
            ])
        );
        assert_eq!(plain_out[2], parse("(list \"yo\" bob bang)"));
    }

    /// A value the CALL never supplied has no source position of its own,
    /// so `MacroArgCarrier::lift_default` stamps it at the call site rather
    /// than leaving it synthetic. Pins the `Site = Span` choice.
    #[test]
    fn absent_optional_default_wears_the_call_site_span() {
        let src = "(defmacro f (a &optional (b 7)) `(list ,a ,b)) (f 1)";
        let out = SpannedExpander::new()
            .expand_program(read_spanned(src).unwrap())
            .unwrap();
        let SpannedForm::List(children) = &out[0].form else {
            panic!("expected a list")
        };
        let call_start = src.rfind("(f 1)").unwrap();
        let call_span = Span::new(call_start, call_start + "(f 1)".len());
        // `,b` was never supplied — it wears the call span, not a synthetic
        // one, and not the definition-site span of the `7` literal.
        assert_eq!(children[2].to_sexp(), Sexp::int(7));
        assert!(!children[2].span.is_synthetic());
        assert_eq!(children[2].span, call_span);
    }

    /// Surplus args against a rest-less param list are a rejection on BOTH
    /// paths. Before step 5c the spanned binder silently dropped them while
    /// the plain binder raised `TooManyMacroArgs` — the exact class of
    /// two-implementations divergence the shared binder removes.
    #[test]
    fn surplus_args_rejected_like_plain_expander() {
        use crate::macro_expand::Expander;

        let src = "(defmacro two (a b) `(list ,a ,b)) (two 1 2 3)";
        let plain = Expander::new().expand_program(read(src).unwrap());
        let spanned = SpannedExpander::new().expand_program(read_spanned(src).unwrap());
        assert!(plain.is_err(), "plain expander accepted a surplus arg");
        assert!(spanned.is_err(), "spanned expander accepted a surplus arg");
    }

    /// A malformed lambda list is rejected identically on both paths,
    /// because there is exactly one `parse_params`.
    #[test]
    fn malformed_lambda_list_rejected_like_plain_expander() {
        use crate::macro_expand::Expander;

        for src in [
            // `&rest` with no name.
            "(defmacro f (a &rest) `(list ,a))",
            // tokens trailing the `&rest` name.
            "(defmacro f (a &rest r junk) `(list ,a))",
            // `(name default)` optional spec with no default.
            "(defmacro f (&optional (b)) `(list ,b))",
            // non-symbol in the required run.
            "(defmacro f (1) `(list))",
        ] {
            let plain = Expander::new().expand_program(read(src).unwrap());
            let spanned = SpannedExpander::new().expand_program(read_spanned(src).unwrap());
            assert!(plain.is_err(), "plain expander accepted: {src}");
            assert!(spanned.is_err(), "spanned expander accepted: {src}");
        }
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
