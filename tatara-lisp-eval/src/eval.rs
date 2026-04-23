//! Core evaluator.
//!
//! Threads a mutable `Env` and the immutable `FnRegistry<H>` through
//! recursive eval. Special forms are dispatched by head symbol before
//! function application. Closures capture a snapshot of the current env
//! at lambda creation; native functions live in the registry and are
//! referred to in values by name.

use std::sync::Arc;

use tatara_lisp::{Atom, Span, Spanned, SpannedForm};

use crate::env::Env;
use crate::error::{EvalError, Result};
use crate::ffi::{Arity, FnEntry, FnRegistry, NativeCallable};
use crate::special::SpecialForm;
use crate::value::{Closure, NativeFn, Value};

/// An embedded tatara-lisp evaluator, parameterized over the host context
/// `H` that registered functions read/write.
pub struct Interpreter<H> {
    pub(crate) registry: FnRegistry<H>,
    pub(crate) globals: Env,
}

impl<H: 'static> Interpreter<H> {
    pub fn new() -> Self {
        Self {
            registry: FnRegistry::new(),
            globals: Env::new(),
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
    /// global environment.
    pub fn eval_spanned(&mut self, form: &Spanned, host: &mut H) -> Result<Value> {
        eval_in(&mut self.globals, &self.registry, form, host)
    }

    /// Evaluate a slice of forms in order, returning the last result.
    /// Empty input returns `Value::Nil`.
    pub fn eval_program(&mut self, forms: &[Spanned], host: &mut H) -> Result<Value> {
        let mut last = Value::Nil;
        for form in forms {
            last = self.eval_spanned(form, host)?;
        }
        Ok(last)
    }

    /// Look up a symbol in the global env.
    pub fn lookup_global(&self, name: &str) -> Option<Value> {
        self.globals.lookup(name)
    }

    /// Bind a value in the global env.
    pub fn define_global(&mut self, name: impl Into<Arc<str>>, value: Value) {
        self.globals.define(name, value);
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
        SpannedForm::Quasiquote(_) => Err(EvalError::NotImplemented("quasiquote (Phase 2.5+)")),
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
        SpecialForm::Quasiquote => Err(EvalError::NotImplemented("quasiquote (Phase 2.5+)")),
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
}
