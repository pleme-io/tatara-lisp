//! Core evaluator.
//!
//! Threads a mutable `Env` and the immutable `FnRegistry<H>` through
//! recursive eval. Special forms are dispatched by head symbol before
//! function application. Closures capture a snapshot of the current env
//! at lambda creation; native functions live in the registry and are
//! referred to in values by name.

use std::sync::Arc;

use tatara_lisp::{Atom, Span, Spanned, SpannedExpander, SpannedForm};

use crate::env::Env;
use crate::error::{EvalError, Result};
use crate::ffi::{Arity, FnEntry, FnRegistry, FromValue, IntoValue, NativeCallable};
use crate::special::SpecialForm;
use crate::value::{Closure, NativeFn, Value};

/// An embedded tatara-lisp evaluator, parameterized over the host context
/// `H` that registered functions read/write.
pub struct Interpreter<H> {
    pub(crate) registry: FnRegistry<H>,
    pub(crate) globals: Env,
    /// Span-preserving macro expander. Top-level `defmacro`,
    /// `defpoint-template`, and `defcheck` forms register here; macro calls
    /// in subsequent forms are rewritten before evaluation. Persisted across
    /// `eval_program` calls so REPL sessions accumulate macros naturally.
    pub(crate) expander: SpannedExpander,
}

impl<H: 'static> Interpreter<H> {
    pub fn new() -> Self {
        Self {
            registry: FnRegistry::new(),
            globals: Env::new(),
            expander: SpannedExpander::new(),
        }
    }

    /// Register a native Rust function, exposing it to Lisp code under
    /// `name`. Re-registering the same name overwrites the prior entry
    /// (last-write-wins) and leaves the global binding intact.
    pub fn register_fn<F>(&mut self, name: impl Into<Arc<str>>, arity: Arity, callable: F)
    where
        F: NativeCallable<H>,
    {
        let name = name.into();
        self.registry.insert(FnEntry {
            name: name.clone(),
            arity,
            callable: Box::new(callable),
        });
        self.globals.define(
            name.clone(),
            Value::NativeFn(Arc::new(NativeFn { name, arity })),
        );
    }

    /// Evaluate a single already-read spanned form in this interpreter's
    /// global environment. Macro expansion is applied first when the
    /// expander has at least one macro registered — registration of
    /// `defmacro` is a top-level concern handled by `eval_program` /
    /// `eval_top_form`; bare `eval_spanned` skips it.
    pub fn eval_spanned(&mut self, form: &Spanned, host: &mut H) -> Result<Value> {
        let expanded;
        let target = if self.expander.is_empty() {
            form
        } else {
            expanded = self.expander.expand(form)?;
            &expanded
        };
        eval_in(&mut self.globals, &self.registry, target, host)
    }

    /// Evaluate a slice of forms in order, returning the last result.
    ///
    /// Top-level `defmacro` / `defpoint-template` / `defcheck` forms register
    /// into the persistent expander and yield `Value::Nil`. All other forms
    /// are run through `expander.expand` (rewriting any registered macro
    /// calls anywhere in the form tree) before being evaluated. This is the
    /// canonical entry point for running a tatara-lisp program — REPL,
    /// embedded host, batch script.
    ///
    /// Empty input returns `Value::Nil`.
    pub fn eval_program(&mut self, forms: &[Spanned], host: &mut H) -> Result<Value> {
        let mut last = Value::Nil;
        for form in forms {
            last = self.eval_top_form(form, host)?;
        }
        Ok(last)
    }

    /// Evaluate one top-level form: register macros, expand, then eval.
    /// Public so embedders that drive the read-eval loop themselves
    /// (REPL, hot-reload watchers) can preserve top-level semantics
    /// without re-implementing the registration handshake.
    pub fn eval_top_form(&mut self, form: &Spanned, host: &mut H) -> Result<Value> {
        if self.expander.try_register_macro(form)? {
            return Ok(Value::Nil);
        }
        let expanded;
        let target = if self.expander.is_empty() {
            form
        } else {
            expanded = self.expander.expand(form)?;
            &expanded
        };
        eval_in(&mut self.globals, &self.registry, target, host)
    }

    /// Borrow the macro expander. Embedders may register macros directly
    /// (e.g. preloaded standard library) without reading them from source.
    pub fn expander(&self) -> &SpannedExpander {
        &self.expander
    }

    /// Mutable access to the expander — for preloading macros via
    /// `try_register_macro` from a separately-read form list, or clearing
    /// the registry.
    pub fn expander_mut(&mut self) -> &mut SpannedExpander {
        &mut self.expander
    }

    /// Look up a symbol in the global env.
    pub fn lookup_global(&self, name: &str) -> Option<Value> {
        self.globals.lookup(name)
    }

    /// Bind a value in the global env.
    pub fn define_global(&mut self, name: impl Into<Arc<str>>, value: Value) {
        self.globals.define(name, value);
    }

    // ── Typed registration helpers ──────────────────────────────────

    /// Register a 0-arity native fn with typed return value.
    pub fn register_typed0<R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        R: IntoValue + 'static,
        F: Fn(&mut H) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(0),
            move |_args: &[Value], host: &mut H, _sp| f(host).map(IntoValue::into_value),
        );
    }

    /// Register a 1-arity native fn with typed arg + return.
    pub fn register_typed1<A, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(1),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                f(host, a).map(IntoValue::into_value)
            },
        );
    }

    /// Register a 2-arity native fn with typed args + return.
    pub fn register_typed2<A, B, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        B: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A, B) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(2),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                let b = B::from_value(&args[1], sp)?;
                f(host, a, b).map(IntoValue::into_value)
            },
        );
    }

    /// Register a 3-arity native fn with typed args + return.
    pub fn register_typed3<A, B, C, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        B: FromValue + 'static,
        C: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A, B, C) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(3),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                let b = B::from_value(&args[1], sp)?;
                let c = C::from_value(&args[2], sp)?;
                f(host, a, b, c).map(IntoValue::into_value)
            },
        );
    }

    /// Register a 4-arity native fn with typed args + return.
    pub fn register_typed4<A, B, C, D, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        B: FromValue + 'static,
        C: FromValue + 'static,
        D: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A, B, C, D) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(4),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                let b = B::from_value(&args[1], sp)?;
                let c = C::from_value(&args[2], sp)?;
                let d = D::from_value(&args[3], sp)?;
                f(host, a, b, c, d).map(IntoValue::into_value)
            },
        );
    }
}

impl<H: 'static> Default for Interpreter<H> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Core recursive evaluator ──────────────────────────────────────────

/// Evaluate `form` against `env`, resolving native fns via `registry`.
/// Mutates `env` for `define` / `set!` / body frame push+pop.
pub(crate) fn eval_in<H: 'static>(
    env: &mut Env,
    registry: &FnRegistry<H>,
    form: &Spanned,
    host: &mut H,
) -> Result<Value> {
    match &form.form {
        SpannedForm::Nil => Ok(Value::Nil),
        SpannedForm::Atom(a) => eval_atom(a, form.span, env),
        SpannedForm::Quote(inner) => Ok(quoted_value(inner)),
        SpannedForm::Quasiquote(inner) => quasiquote_eval(inner, env, registry, host),
        SpannedForm::Unquote(_) | SpannedForm::UnquoteSplice(_) => Err(EvalError::bad_form(
            "unquote",
            "unquote outside of quasiquote",
            form.span,
        )),
        SpannedForm::List(items) => {
            if items.is_empty() {
                return Ok(Value::Nil);
            }
            // Head may be a special-form keyword, a symbol that resolves
            // to a callable, or an arbitrary expression that evaluates
            // to a callable.
            if let Some(head_sym) = items[0].as_symbol() {
                if let Some(sf) = SpecialForm::from_symbol(head_sym) {
                    return eval_special(sf, items, form.span, env, registry, host);
                }
            }
            eval_application(items, form.span, env, registry, host)
        }
    }
}

fn eval_atom(a: &Atom, span: Span, env: &Env) -> Result<Value> {
    match a {
        Atom::Symbol(name) => env
            .lookup(name)
            .ok_or_else(|| EvalError::unbound(name.as_str(), span)),
        Atom::Keyword(s) => Ok(Value::Keyword(Arc::from(s.as_str()))),
        Atom::Str(s) => Ok(Value::Str(Arc::from(s.as_str()))),
        Atom::Int(n) => Ok(Value::Int(*n)),
        Atom::Float(n) => Ok(Value::Float(*n)),
        Atom::Bool(b) => Ok(Value::Bool(*b)),
    }
}

/// `(quote x)` yields the source form `x` unevaluated, carrying its span
/// so consumers can pattern-match on structure.
fn quoted_value(inner: &Spanned) -> Value {
    Value::Sexp(inner.to_sexp(), inner.span)
}

/// Evaluate a quasiquoted form — unlike `quote`, `,expr` inside the form
/// is evaluated and substituted, and `,@expr` splices the evaluated list
/// into the enclosing list. Atoms lower to their runtime `Value`
/// equivalents (Symbol → Value::Symbol, etc.). Nested quasiquote is not
/// supported in v1 — it is returned as an opaque `Value::Sexp` literal.
fn quasiquote_eval<H: 'static>(
    form: &Spanned,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    match &form.form {
        SpannedForm::Unquote(inner) => eval_in(env, registry, inner, host),
        SpannedForm::UnquoteSplice(_) => Err(EvalError::bad_form(
            "unquote-splice",
            "`,@` only valid directly inside a list",
            form.span,
        )),
        SpannedForm::List(items) => {
            let mut out: Vec<Value> = Vec::with_capacity(items.len());
            for item in items {
                if let SpannedForm::UnquoteSplice(inner) = &item.form {
                    let v = eval_in(env, registry, inner, host)?;
                    match v {
                        Value::List(xs) => out.extend(xs.iter().cloned()),
                        Value::Nil => {}
                        other => {
                            return Err(EvalError::type_mismatch(
                                "list",
                                other.type_name(),
                                item.span,
                            ))
                        }
                    }
                } else {
                    out.push(quasiquote_eval(item, env, registry, host)?);
                }
            }
            if out.is_empty() {
                Ok(Value::Nil)
            } else {
                Ok(Value::list(out))
            }
        }
        SpannedForm::Nil => Ok(Value::Nil),
        SpannedForm::Atom(a) => Ok(match a {
            Atom::Symbol(s) => Value::Symbol(Arc::from(s.as_str())),
            Atom::Keyword(s) => Value::Keyword(Arc::from(s.as_str())),
            Atom::Str(s) => Value::Str(Arc::from(s.as_str())),
            Atom::Int(n) => Value::Int(*n),
            Atom::Float(n) => Value::Float(*n),
            Atom::Bool(b) => Value::Bool(*b),
        }),
        // Inside quasiquote, an inner `quote` is preserved structurally —
        // we treat it as an opaque literal subtree so downstream consumers
        // can see it as a source form if they care.
        SpannedForm::Quote(_) | SpannedForm::Quasiquote(_) => {
            Ok(Value::Sexp(form.to_sexp(), form.span))
        }
    }
}

// ── Function application ──────────────────────────────────────────────

fn eval_application<H: 'static>(
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    let head_val = eval_in(env, registry, &items[0], host)?;
    let mut args: Vec<Value> = Vec::with_capacity(items.len().saturating_sub(1));
    for arg_form in &items[1..] {
        args.push(eval_in(env, registry, arg_form, host)?);
    }
    apply(&head_val, args, call_span, registry, host)
}

fn apply<H: 'static>(
    callee: &Value,
    args: Vec<Value>,
    call_span: Span,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    match callee {
        Value::NativeFn(nfn) => {
            if nfn.arity.check(args.len()).is_err() {
                return Err(EvalError::ArityMismatch {
                    fn_name: nfn.name.clone(),
                    expected: nfn.arity,
                    got: args.len(),
                    at: call_span,
                });
            }
            let entry = registry.lookup(&nfn.name).ok_or_else(|| {
                EvalError::native_fn(
                    nfn.name.clone(),
                    format!("native fn {} is not registered", nfn.name),
                    call_span,
                )
            })?;
            entry.callable.call(&args, host, call_span)
        }
        Value::Closure(c) => call_closure(c, args, call_span, registry, host),
        other => Err(EvalError::NotCallable {
            value_kind: other.type_name(),
            at: call_span,
        }),
    }
}

fn call_closure<H: 'static>(
    closure: &Closure,
    args: Vec<Value>,
    call_span: Span,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    let required = closure.params.len();
    let has_rest = closure.rest.is_some();
    if !has_rest && args.len() != required {
        return Err(EvalError::ArityMismatch {
            fn_name: Arc::from("<closure>"),
            expected: Arity::Exact(required),
            got: args.len(),
            at: call_span,
        });
    }
    if has_rest && args.len() < required {
        return Err(EvalError::ArityMismatch {
            fn_name: Arc::from("<closure>"),
            expected: Arity::AtLeast(required),
            got: args.len(),
            at: call_span,
        });
    }

    let mut env = closure.captured_env.clone();
    env.push();
    for (param, arg) in closure.params.iter().zip(args.iter()) {
        env.define(param.clone(), arg.clone());
    }
    if let Some(rest_name) = &closure.rest {
        let rest_args: Vec<Value> = args.iter().skip(required).cloned().collect();
        env.define(rest_name.clone(), Value::list(rest_args));
    }

    let mut result = Value::Nil;
    for body_form in &closure.body {
        result = eval_in(&mut env, registry, body_form, host)?;
    }
    Ok(result)
}

// ── Special forms ─────────────────────────────────────────────────────

fn eval_special<H: 'static>(
    sf: SpecialForm,
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    match sf {
        SpecialForm::Quote => sf_quote(items, call_span),
        SpecialForm::Quasiquote => {
            if items.len() != 2 {
                return Err(EvalError::bad_form(
                    "quasiquote",
                    format!("expected 1 arg, got {}", items.len() - 1),
                    call_span,
                ));
            }
            quasiquote_eval(&items[1], env, registry, host)
        }
        SpecialForm::If => sf_if(items, call_span, env, registry, host),
        SpecialForm::Cond => sf_cond(items, call_span, env, registry, host),
        SpecialForm::When => sf_when_unless(items, call_span, env, registry, host, false),
        SpecialForm::Unless => sf_when_unless(items, call_span, env, registry, host, true),
        SpecialForm::Let => sf_let(items, call_span, env, registry, host),
        SpecialForm::LetStar => sf_let_star(items, call_span, env, registry, host),
        SpecialForm::LetRec => sf_letrec(items, call_span, env, registry, host),
        SpecialForm::Lambda => sf_lambda(items, call_span, env),
        SpecialForm::Define => sf_define(items, call_span, env, registry, host),
        SpecialForm::Set => sf_set(items, call_span, env, registry, host),
        SpecialForm::Begin => sf_begin(&items[1..], env, registry, host),
        SpecialForm::And => sf_and(&items[1..], env, registry, host),
        SpecialForm::Or => sf_or(&items[1..], env, registry, host),
        SpecialForm::Not => sf_not(items, call_span, env, registry, host),
    }
}

fn sf_quote(items: &[Spanned], span: Span) -> Result<Value> {
    if items.len() != 2 {
        return Err(EvalError::bad_form(
            "quote",
            format!("expected 1 arg, got {}", items.len() - 1),
            span,
        ));
    }
    Ok(Value::Sexp(items[1].to_sexp(), items[1].span))
}

fn sf_if<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 || items.len() > 4 {
        return Err(EvalError::bad_form(
            "if",
            format!("expected (if c t [e]), got {} subforms", items.len()),
            span,
        ));
    }
    let c = eval_in(env, registry, &items[1], host)?;
    if c.is_truthy() {
        eval_in(env, registry, &items[2], host)
    } else if items.len() == 4 {
        eval_in(env, registry, &items[3], host)
    } else {
        Ok(Value::Nil)
    }
}

fn sf_cond<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    for clause in &items[1..] {
        let Some(clause_list) = clause.as_list() else {
            return Err(EvalError::bad_form(
                "cond",
                "clause must be a list",
                clause.span,
            ));
        };
        if clause_list.is_empty() {
            return Err(EvalError::bad_form("cond", "empty clause", clause.span));
        }
        let is_else = clause_list[0].as_symbol() == Some("else");
        let cond_matches = if is_else {
            true
        } else {
            let v = eval_in(env, registry, &clause_list[0], host)?;
            v.is_truthy()
        };
        if cond_matches {
            let mut last = Value::Nil;
            for expr in &clause_list[1..] {
                last = eval_in(env, registry, expr, host)?;
            }
            return Ok(last);
        }
    }
    // No clause matched.
    let _ = span;
    Ok(Value::Nil)
}

fn sf_when_unless<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
    invert: bool,
) -> Result<Value> {
    if items.len() < 2 {
        return Err(EvalError::bad_form(
            if invert { "unless" } else { "when" },
            "need a test",
            span,
        ));
    }
    let cond = eval_in(env, registry, &items[1], host)?;
    let run = cond.is_truthy() ^ invert;
    if run {
        let mut last = Value::Nil;
        for expr in &items[2..] {
            last = eval_in(env, registry, expr, host)?;
        }
        Ok(last)
    } else {
        Ok(Value::Nil)
    }
}

/// Parse a `((name expr) ...)` binding list into `[(name, &expr_spanned)]`.
fn parse_binding_list<'a>(
    list: &'a Spanned,
    form_name: &'static str,
) -> Result<Vec<(Arc<str>, &'a Spanned)>> {
    let bindings = list
        .as_list()
        .ok_or_else(|| EvalError::bad_form(form_name, "bindings must be a list", list.span))?;
    let mut out = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let pair = binding.as_list().ok_or_else(|| {
            EvalError::bad_form(form_name, "each binding must be (name expr)", binding.span)
        })?;
        if pair.len() != 2 {
            return Err(EvalError::bad_form(
                form_name,
                "binding must be exactly (name expr)",
                binding.span,
            ));
        }
        let name = pair[0].as_symbol().ok_or_else(|| {
            EvalError::bad_form(form_name, "binding name must be a symbol", pair[0].span)
        })?;
        out.push((Arc::<str>::from(name), &pair[1]));
    }
    Ok(out)
}

fn sf_let<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "let",
            "expected (let ((name expr)...) body...)",
            span,
        ));
    }
    let bindings = parse_binding_list(&items[1], "let")?;
    // Parallel semantics: evaluate all RHS in the *outer* env, then
    // extend with new frame.
    let mut values = Vec::with_capacity(bindings.len());
    for (_, expr) in &bindings {
        values.push(eval_in(env, registry, expr, host)?);
    }
    env.push();
    for ((name, _), val) in bindings.into_iter().zip(values) {
        env.define(name, val);
    }
    let result = eval_body(&items[2..], env, registry, host);
    env.pop();
    result
}

fn sf_let_star<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "let*",
            "expected (let* ((name expr)...) body...)",
            span,
        ));
    }
    let bindings = parse_binding_list(&items[1], "let*")?;
    env.push();
    for (name, expr) in bindings {
        let v = eval_in(env, registry, expr, host)?;
        env.define(name, v);
    }
    let result = eval_body(&items[2..], env, registry, host);
    env.pop();
    result
}

fn sf_letrec<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "letrec",
            "expected (letrec ((name expr)...) body...)",
            span,
        ));
    }
    let bindings = parse_binding_list(&items[1], "letrec")?;
    env.push();
    // Pre-bind each name to Nil so RHS can self-reference (and cross-
    // reference). Then eval each RHS in order and rebind.
    for (name, _) in &bindings {
        env.define(name.clone(), Value::Nil);
    }
    for (name, expr) in &bindings {
        let v = eval_in(env, registry, expr, host)?;
        env.define(name.clone(), v);
    }
    let result = eval_body(&items[2..], env, registry, host);
    env.pop();
    result
}

fn eval_body<H: 'static>(
    body: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    let mut last = Value::Nil;
    for form in body {
        last = eval_in(env, registry, form, host)?;
    }
    Ok(last)
}

fn sf_lambda(items: &[Spanned], span: Span, env: &Env) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "lambda",
            "expected (lambda (params...) body...)",
            span,
        ));
    }
    let param_list = items[1]
        .as_list()
        .ok_or_else(|| EvalError::bad_form("lambda", "params must be a list", items[1].span))?;
    let (params, rest) = parse_lambda_params(param_list, items[1].span)?;
    let body = items[2..].to_vec();
    Ok(Value::Closure(Arc::new(Closure {
        params,
        rest,
        body,
        captured_env: env.clone(),
        source: span,
    })))
}

fn parse_lambda_params(list: &[Spanned], span: Span) -> Result<(Vec<Arc<str>>, Option<Arc<str>>)> {
    let mut params = Vec::new();
    let mut rest = None;
    let mut i = 0;
    while i < list.len() {
        let s = list[i]
            .as_symbol()
            .ok_or_else(|| EvalError::bad_form("lambda", "param must be a symbol", list[i].span))?;
        if s == "&rest" {
            let name = list
                .get(i + 1)
                .and_then(Spanned::as_symbol)
                .ok_or_else(|| EvalError::bad_form("lambda", "&rest needs a name", span))?;
            rest = Some(Arc::<str>::from(name));
            if i + 2 != list.len() {
                return Err(EvalError::bad_form(
                    "lambda",
                    "&rest must be the last param",
                    span,
                ));
            }
            break;
        }
        params.push(Arc::<str>::from(s));
        i += 1;
    }
    Ok((params, rest))
}

/// `(define name expr)` or `(define (name params...) body...)`
fn sf_define<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "define",
            "expected (define name expr) or (define (name args) body)",
            span,
        ));
    }
    match &items[1].form {
        SpannedForm::Atom(Atom::Symbol(name)) => {
            let v = eval_in(env, registry, &items[2], host)?;
            env.define(Arc::<str>::from(name.as_str()), v);
            Ok(Value::Nil)
        }
        SpannedForm::List(head_list) => {
            if head_list.is_empty() {
                return Err(EvalError::bad_form(
                    "define",
                    "empty (name args) list",
                    items[1].span,
                ));
            }
            let name = head_list[0].as_symbol().ok_or_else(|| {
                EvalError::bad_form(
                    "define",
                    "first item in (name args) must be a symbol",
                    head_list[0].span,
                )
            })?;
            let (params, rest) = parse_lambda_params(&head_list[1..], items[1].span)?;
            let body = items[2..].to_vec();
            let closure = Arc::new(Closure {
                params,
                rest,
                body,
                captured_env: env.clone(),
                source: span,
            });
            env.define(Arc::<str>::from(name), Value::Closure(closure));
            Ok(Value::Nil)
        }
        _ => Err(EvalError::bad_form(
            "define",
            "second form must be a symbol or (name args) list",
            items[1].span,
        )),
    }
}

fn sf_set<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    if items.len() != 3 {
        return Err(EvalError::bad_form(
            "set!",
            "expected (set! name expr)",
            span,
        ));
    }
    let name = items[1]
        .as_symbol()
        .ok_or_else(|| EvalError::bad_form("set!", "first arg must be a symbol", items[1].span))?;
    let v = eval_in(env, registry, &items[2], host)?;
    if env.set(name, v) {
        Ok(Value::Nil)
    } else {
        Err(EvalError::unbound(name, items[1].span))
    }
}

fn sf_begin<H: 'static>(
    body: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    eval_body(body, env, registry, host)
}

fn sf_and<H: 'static>(
    exprs: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    let mut last = Value::Bool(true);
    for e in exprs {
        last = eval_in(env, registry, e, host)?;
        if !last.is_truthy() {
            return Ok(last);
        }
    }
    Ok(last)
}

fn sf_or<H: 'static>(
    exprs: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    let mut last = Value::Bool(false);
    for e in exprs {
        last = eval_in(env, registry, e, host)?;
        if last.is_truthy() {
            return Ok(last);
        }
    }
    Ok(last)
}

fn sf_not<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    host: &mut H,
) -> Result<Value> {
    if items.len() != 2 {
        return Err(EvalError::bad_form("not", "expected (not x)", span));
    }
    let v = eval_in(env, registry, &items[1], host)?;
    Ok(Value::Bool(!v.is_truthy()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::install_primitives;
    use tatara_lisp::{read_spanned, Sexp};

    struct NoHost;

    fn eval_ok(src: &str) -> Value {
        let forms = read_spanned(src).unwrap();
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        i.eval_program(&forms, &mut host).unwrap()
    }

    fn eval_err(src: &str) -> EvalError {
        let forms = read_spanned(src).unwrap();
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        i.eval_program(&forms, &mut host).unwrap_err()
    }

    // ── Literals + symbol lookup ──────────────────────────────────

    #[test]
    fn literal_int() {
        assert!(matches!(eval_ok("42"), Value::Int(42)));
    }

    #[test]
    fn unbound_symbol_errors() {
        let e = eval_err("no-such-var");
        assert!(matches!(e, EvalError::UnboundSymbol { .. }));
    }

    #[test]
    fn quote_returns_source_form() {
        let v = eval_ok("'(a b c)");
        match v {
            Value::Sexp(Sexp::List(xs), _) => assert_eq!(xs.len(), 3),
            other => panic!("{other:?}"),
        }
    }

    // ── Arithmetic via primitives ─────────────────────────────────

    #[test]
    fn add_ints() {
        assert!(matches!(eval_ok("(+ 1 2 3)"), Value::Int(6)));
    }

    #[test]
    fn sub_divides_float() {
        match eval_ok("(- 10 3)") {
            Value::Int(7) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn division_by_zero_errors() {
        assert!(matches!(
            eval_err("(/ 1 0)"),
            EvalError::DivisionByZero { .. }
        ));
    }

    // ── Conditionals ──────────────────────────────────────────────

    #[test]
    fn if_truthy_branch() {
        assert!(matches!(eval_ok("(if #t 1 2)"), Value::Int(1)));
    }

    #[test]
    fn if_falsy_branch() {
        assert!(matches!(eval_ok("(if #f 1 2)"), Value::Int(2)));
    }

    #[test]
    fn if_no_else_returns_nil() {
        assert!(matches!(eval_ok("(if #f 1)"), Value::Nil));
    }

    #[test]
    fn cond_picks_first_match() {
        assert!(matches!(
            eval_ok("(cond (#f 1) (#t 2) (else 3))"),
            Value::Int(2)
        ));
    }

    #[test]
    fn cond_falls_through_to_else() {
        assert!(matches!(
            eval_ok("(cond (#f 1) (#f 2) (else 3))"),
            Value::Int(3)
        ));
    }

    #[test]
    fn when_runs_body_if_true() {
        assert!(matches!(eval_ok("(when #t 99)"), Value::Int(99)));
        assert!(matches!(eval_ok("(when #f 99)"), Value::Nil));
    }

    // ── Let forms ─────────────────────────────────────────────────

    #[test]
    fn let_binds_and_evaluates_body() {
        assert!(matches!(
            eval_ok("(let ((x 10) (y 20)) (+ x y))"),
            Value::Int(30)
        ));
    }

    #[test]
    fn let_star_sequential_bindings() {
        assert!(matches!(
            eval_ok("(let* ((x 5) (y (+ x 1))) (+ x y))"),
            Value::Int(11)
        ));
    }

    #[test]
    fn letrec_mutual_recursion() {
        let v = eval_ok(
            "(letrec ((even? (lambda (n) (if (= n 0) #t (odd? (- n 1)))))
                      (odd?  (lambda (n) (if (= n 0) #f (even? (- n 1))))))
               (even? 10))",
        );
        assert!(matches!(v, Value::Bool(true)));
    }

    // ── Lambda + closure ──────────────────────────────────────────

    #[test]
    fn lambda_applies() {
        assert!(matches!(
            eval_ok("((lambda (x y) (+ x y)) 3 4)"),
            Value::Int(7)
        ));
    }

    #[test]
    fn lambda_closes_over_env() {
        assert!(matches!(
            eval_ok("(let ((n 10)) ((lambda (x) (+ x n)) 5))"),
            Value::Int(15)
        ));
    }

    #[test]
    fn closure_captures_by_value_at_creation() {
        // make-adder style — the returned closure should capture n=5 even
        // though the outer let scope has exited.
        let v = eval_ok(
            "(define make-adder (lambda (n) (lambda (x) (+ x n))))
             (define add5 (make-adder 5))
             (add5 10)",
        );
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn rest_args_collect_into_list() {
        let v = eval_ok("((lambda (x &rest rs) (length rs)) 1 2 3 4 5)");
        assert!(matches!(v, Value::Int(4)));
    }

    #[test]
    fn closure_arity_mismatch() {
        let e = eval_err("((lambda (x y) (+ x y)) 1)");
        assert!(matches!(e, EvalError::ArityMismatch { .. }));
    }

    // ── Define + set! ─────────────────────────────────────────────

    #[test]
    fn define_then_use() {
        assert!(matches!(eval_ok("(define x 42) x"), Value::Int(42)));
    }

    #[test]
    fn define_function_shorthand() {
        assert!(matches!(
            eval_ok("(define (sq x) (* x x)) (sq 6)"),
            Value::Int(36)
        ));
    }

    #[test]
    fn set_mutates_existing() {
        assert!(matches!(
            eval_ok("(define x 1) (set! x 99) x"),
            Value::Int(99)
        ));
    }

    #[test]
    fn set_unbound_errors() {
        let e = eval_err("(set! nope 1)");
        assert!(matches!(e, EvalError::UnboundSymbol { .. }));
    }

    // ── begin / and / or / not ────────────────────────────────────

    #[test]
    fn begin_returns_last() {
        assert!(matches!(eval_ok("(begin 1 2 3)"), Value::Int(3)));
    }

    #[test]
    fn and_short_circuits() {
        assert!(matches!(eval_ok("(and 1 #f 2)"), Value::Bool(false)));
        assert!(matches!(eval_ok("(and 1 2 3)"), Value::Int(3)));
        assert!(matches!(eval_ok("(and)"), Value::Bool(true)));
    }

    #[test]
    fn or_short_circuits() {
        assert!(matches!(eval_ok("(or #f #f 7)"), Value::Int(7)));
        assert!(matches!(eval_ok("(or #f #f)"), Value::Bool(false)));
        assert!(matches!(eval_ok("(or)"), Value::Bool(false)));
    }

    #[test]
    fn not_inverts() {
        assert!(matches!(eval_ok("(not #t)"), Value::Bool(false)));
        assert!(matches!(eval_ok("(not #f)"), Value::Bool(true)));
        assert!(matches!(eval_ok("(not 42)"), Value::Bool(false)));
    }

    // ── Recursion ─────────────────────────────────────────────────

    #[test]
    fn recursive_factorial() {
        let v = eval_ok(
            "(define (fact n)
               (if (= n 0) 1 (* n (fact (- n 1)))))
             (fact 6)",
        );
        assert!(matches!(v, Value::Int(720)));
    }

    #[test]
    fn recursive_length() {
        let v = eval_ok(
            "(define (len xs)
               (if (null? xs) 0 (+ 1 (len (cdr xs)))))
             (len (list 1 2 3 4 5))",
        );
        assert!(matches!(v, Value::Int(5)));
    }

    // ── Host context reachable via register_fn ────────────────────

    // ── Quasiquote ────────────────────────────────────────────────

    #[test]
    fn quasiquote_plain_list_is_runtime_list() {
        let v = eval_ok("`(a b c)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[0], Value::Symbol(s) if s.as_ref() == "a"));
                assert!(matches!(&xs[1], Value::Symbol(s) if s.as_ref() == "b"));
                assert!(matches!(&xs[2], Value::Symbol(s) if s.as_ref() == "c"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_unquote_substitutes_evaluated_value() {
        let v = eval_ok("(let ((x 42)) `(a ,x c))");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[1], Value::Int(42)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_unquote_arbitrary_expr() {
        let v = eval_ok("`(x ,(+ 1 2 3) y)");
        match v {
            Value::List(xs) => {
                assert!(matches!(&xs[1], Value::Int(6)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_splice_inlines_list() {
        let v = eval_ok("`(a ,@(list 1 2 3) b)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 5);
                assert!(matches!(&xs[0], Value::Symbol(s) if s.as_ref() == "a"));
                assert!(matches!(&xs[1], Value::Int(1)));
                assert!(matches!(&xs[2], Value::Int(2)));
                assert!(matches!(&xs[3], Value::Int(3)));
                assert!(matches!(&xs[4], Value::Symbol(s) if s.as_ref() == "b"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_splice_empty_list_splices_nothing() {
        let v = eval_ok("`(a ,@(list) b)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 2);
                assert!(matches!(&xs[0], Value::Symbol(s) if s.as_ref() == "a"));
                assert!(matches!(&xs[1], Value::Symbol(s) if s.as_ref() == "b"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_splice_non_list_errors() {
        let e = eval_err("`(a ,@42)");
        assert!(matches!(e, EvalError::TypeMismatch { .. }));
    }

    #[test]
    fn quasiquote_atom_yields_atom_value() {
        assert!(matches!(eval_ok("`foo"), Value::Symbol(s) if s.as_ref() == "foo"));
        assert!(matches!(eval_ok("`42"), Value::Int(42)));
    }

    #[test]
    fn quasiquote_with_nested_list_and_unquote() {
        // `(foo (bar ,x) baz) where x=99 → (foo (bar 99) baz)
        let v = eval_ok("(let ((x 99)) `(foo (bar ,x) baz))");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                match &xs[1] {
                    Value::List(inner) => {
                        assert!(matches!(&inner[1], Value::Int(99)));
                    }
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_symbol_keyword_distinction_preserved() {
        let v = eval_ok("`(:key val)");
        match v {
            Value::List(xs) => {
                assert!(matches!(&xs[0], Value::Keyword(s) if s.as_ref() == "key"));
                assert!(matches!(&xs[1], Value::Symbol(s) if s.as_ref() == "val"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bare_unquote_outside_quasiquote_errors() {
        let e = eval_err(",x");
        assert!(matches!(e, EvalError::BadSpecialForm { .. }));
    }

    // ── Host context reachable via register_fn ────────────────────

    #[test]
    fn native_fn_reads_host_state() {
        struct Counter {
            n: i64,
        }
        let forms = read_spanned("(bump) (bump) (bump) (cur)").unwrap();
        let mut i: Interpreter<Counter> = Interpreter::new();
        install_primitives(&mut i);
        i.register_fn(
            "bump",
            Arity::Exact(0),
            |_args: &[Value], host: &mut Counter, _span| {
                host.n += 1;
                Ok(Value::Int(host.n))
            },
        );
        i.register_fn(
            "cur",
            Arity::Exact(0),
            |_args: &[Value], host: &mut Counter, _span| Ok(Value::Int(host.n)),
        );
        let mut host = Counter { n: 0 };
        let v = i.eval_program(&forms, &mut host).unwrap();
        assert!(matches!(v, Value::Int(3)));
    }

    // ── Typed FFI registration ────────────────────────────────────

    struct Ctx {
        records: Vec<(String, i64)>,
    }

    #[test]
    fn register_typed1_marshals_string_arg() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed1("greet", |_h: &mut Ctx, name: String| -> Result<String> {
            Ok(format!("hello {name}"))
        });
        let forms = read_spanned(r#"(greet "luis")"#).unwrap();
        let mut h = Ctx { records: vec![] };
        let v = i.eval_program(&forms, &mut h).unwrap();
        match v {
            Value::Str(s) => assert_eq!(&*s, "hello luis"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn register_typed2_marshals_host_state_mutation() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed2(
            "record",
            |h: &mut Ctx, name: String, n: i64| -> Result<()> {
                h.records.push((name, n));
                Ok(())
            },
        );
        let forms = read_spanned(r#"(record "a" 1) (record "b" 2)"#).unwrap();
        let mut h = Ctx { records: vec![] };
        let _ = i.eval_program(&forms, &mut h).unwrap();
        assert_eq!(h.records.len(), 2);
        assert_eq!(h.records[0], ("a".to_string(), 1));
        assert_eq!(h.records[1], ("b".to_string(), 2));
    }

    #[test]
    fn register_typed_arg_type_mismatch_surfaces_at_call_site() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed1("needs-int", |_h: &mut Ctx, n: i64| -> Result<i64> {
            Ok(n + 1)
        });
        let forms = read_spanned(r#"(needs-int "not-a-number")"#).unwrap();
        let mut h = Ctx { records: vec![] };
        let err = i.eval_program(&forms, &mut h).unwrap_err();
        assert!(matches!(
            err,
            EvalError::TypeMismatch {
                expected: "integer",
                ..
            }
        ));
    }

    #[test]
    fn register_typed3_three_args() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed3(
            "triple-sum",
            |_h: &mut Ctx, a: i64, b: i64, c: i64| -> Result<i64> { Ok(a + b + c) },
        );
        let forms = read_spanned("(triple-sum 10 20 30)").unwrap();
        let mut h = Ctx { records: vec![] };
        let v = i.eval_program(&forms, &mut h).unwrap();
        assert!(matches!(v, Value::Int(60)));
    }

    // ── User macros via defmacro ──────────────────────────────────

    #[test]
    fn user_macro_expands_and_evaluates() {
        let v = eval_ok(
            "(defmacro twice (x) `(* ,x 2))
             (twice 21)",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn user_macro_definition_returns_nil() {
        let v = eval_ok("(defmacro inc (x) `(+ ,x 1))");
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn user_macro_inside_define_body_expands() {
        // (define (f n) (inc n)) — the (inc n) call is rewritten to (+ n 1)
        // before define captures the body.
        let v = eval_ok(
            "(defmacro inc (x) `(+ ,x 1))
             (define (f n) (inc n))
             (f 41)",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn user_macro_with_rest_args_splices() {
        let v = eval_ok(
            "(defmacro sum-all (&rest xs) `(+ ,@xs))
             (sum-all 1 2 3 4 5)",
        );
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn nested_user_macros_compose() {
        let v = eval_ok(
            "(defmacro twice (x) `(* ,x 2))
             (defmacro quad (x) `(twice (twice ,x)))
             (quad 5)",
        );
        assert!(matches!(v, Value::Int(20)));
    }

    #[test]
    fn user_macro_can_expand_to_special_form() {
        // Macros can expand into special forms — `if`, `let`, `lambda`,
        // `define` are all reachable as expansion targets.
        let v = eval_ok(
            "(defmacro guard (test then) `(if ,test ,then 0))
             (guard #t 99)",
        );
        assert!(matches!(v, Value::Int(99)));
    }

    #[test]
    fn user_macro_redefined_replaces_prior_template() {
        let v = eval_ok(
            "(defmacro k () `1)
             (defmacro k () `2)
             (k)",
        );
        assert!(matches!(v, Value::Int(2)));
    }

    #[test]
    fn user_macro_unbound_template_var_errors() {
        // ,y is not a parameter — expander should refuse.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let forms = read_spanned("(defmacro bad (x) `(list ,y)) (bad 1)").unwrap();
        let err = i.eval_program(&forms, &mut NoHost).unwrap_err();
        // Lowered through Reader from LispError::Compile.
        assert!(matches!(err, EvalError::Reader(_)));
    }

    #[test]
    fn defpoint_template_keyword_registers_as_macro() {
        // `defpoint-template` is the typed-DSL spelling of `defmacro` —
        // the runtime should accept both.
        let v = eval_ok(
            "(defpoint-template double (x) `(* ,x 2))
             (double 7)",
        );
        assert!(matches!(v, Value::Int(14)));
    }

    #[test]
    fn defcheck_keyword_registers_as_macro() {
        let v = eval_ok(
            "(defcheck always-7 () `7)
             (always-7)",
        );
        assert!(matches!(v, Value::Int(7)));
    }

    #[test]
    fn macro_call_evaluated_with_runtime_arg() {
        // Macro arg is itself an expression — the substituted expression
        // is evaluated *after* expansion, so the arg's runtime value is
        // what reaches the expanded form.
        let v = eval_ok(
            "(defmacro double (x) `(+ ,x ,x))
             (define n 13)
             (double n)",
        );
        assert!(matches!(v, Value::Int(26)));
    }

    #[test]
    fn macro_persists_across_eval_program_calls() {
        // The expander state outlives a single eval_program call — REPL
        // semantics rely on this.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        let defs = read_spanned("(defmacro inc (x) `(+ ,x 1))").unwrap();
        i.eval_program(&defs, &mut host).unwrap();
        assert_eq!(i.expander().len(), 1);

        let call = read_spanned("(inc 41)").unwrap();
        let v = i.eval_program(&call, &mut host).unwrap();
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn macro_expansion_inside_lambda_body() {
        let v = eval_ok(
            "(defmacro sq (x) `(* ,x ,x))
             ((lambda (n) (sq n)) 9)",
        );
        assert!(matches!(v, Value::Int(81)));
    }

    #[test]
    fn no_macros_registered_keeps_eval_program_a_passthrough() {
        // Sanity: with no macros registered, eval_program should still run
        // every existing test path correctly. Touching the same code as
        // the rest of the suite — this just asserts the optimization
        // we baked in (skip expand when expander is empty) didn't
        // accidentally drop forms.
        let v = eval_ok("(+ 1 2 3)");
        assert!(matches!(v, Value::Int(6)));
    }

    #[test]
    fn eval_top_form_drives_one_form_at_a_time() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        let forms = read_spanned("(defmacro id (x) `,x) (id 42)").unwrap();

        // First form: registers, returns Nil.
        let r0 = i.eval_top_form(&forms[0], &mut host).unwrap();
        assert!(matches!(r0, Value::Nil));

        // Second form: macro expanded → 42.
        let r1 = i.eval_top_form(&forms[1], &mut host).unwrap();
        assert!(matches!(r1, Value::Int(42)));
    }
}
