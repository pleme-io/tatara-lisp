//! Build-time gradual type checking — the static counterpart to
//! `type_check.rs`. Walks a parsed `Spanned` program; for every form
//! that bears a type annotation (`(the type expr)`, `(defn-typed name
//! (...) -> type body...)`, `(declare name type)`), checks that the
//! inferred type of the underlying expression conforms.
//!
//! Gradual: any expression without an annotation infers as `:any` and
//! conforms to everything. Annotated expressions are checked
//! recursively. The pass produces a list of [`TypeDiagnostic`]
//! records; emit them as caixa-lint diagnostics for IDE integration.
//!
//! Inference is intentionally simple — atoms infer to their kind,
//! literal lists infer to `(:list-of T)` over the LUB of element
//! types, and primitive applications consult a small hard-coded
//! signature table. Anything else is `:any`. This is sufficient to
//! catch the most common authoring mistakes without a full
//! Hindley-Milner constraint solver — and matches the gradual-typing
//! philosophy: catch what you can statically, defer the rest to
//! runtime.
//!
//! # Arity checking — the part that works on UNANNOTATED code
//!
//! Gradualness has a sharp edge: because `infer` falls to
//! [`StaticType::Any`] for essentially every non-literal, and `Any`
//! conforms in BOTH directions ([`StaticType::conforms_to`]), a clean
//! typecheck over a corpus with no annotations says very little. Almost
//! nothing in the fleet's `.tlisp` carries a `(the …)` or a `(declare …)`.
//!
//! ARGUMENT COUNT does not have that problem. It does not depend on
//! inference at all: `(define (f a b) …)` states, in ordinary untyped
//! source, that `f` takes two arguments, and `(f 1 2 3)` contradicts it
//! no matter what any of those expressions infer to. `Any`-conforms-
//! both-ways cannot defeat a count. So the arity pass produces real,
//! falsifiable diagnostics against exactly the code the type pass is
//! blind to.
//!
//! Phase 1 checks the COUNT only. `(define (f a b) …)` synthesizes
//! `StaticType::Fn` with two `Any` parameters and an `Any` return; a
//! call site compares `items.len() - 1` against `params.len()` and
//! infers the call as the arrow's return type. Parameter and return
//! TYPES are deliberately not checked yet — with everything inferring
//! `Any` that would be pure noise, and it is a separate phase once
//! `defn-typed` signatures feed the arrow.
//!
//! ## Where the pass abstains, and why
//!
//! A checker that cries wolf gets switched off, so every case it cannot
//! resolve is an abstention rather than a guess:
//!
//! * A signature containing a lambda-list keyword (`&rest`, `&optional`,
//!   …) is VARIADIC. It binds plain [`StaticType::Procedure`] — no arity
//!   claim at all — rather than counting the symbols before the marker.
//! * Local binders SHADOW. `(define (twice f x) (f (f x)))` calls its
//!   own parameter; if a top-level `f` also exists, comparing against
//!   the top-level arity would be a false positive on correct code. The
//!   walker therefore tracks scopes for every binding shape in
//!   [`tatara_lisp::binding_shapes`] — the same table
//!   `tatara_lisp_lint::rules::unbound_symbol` reads, so the two
//!   syntactic walkers cannot disagree about what `catch` binds.
//! * Quoted data is data: `'(f 1 2 3)` is a list, not a call.
//! * An unknown head (a primitive, an import, a macro) infers `Any` and
//!   is never arity-checked. Only functions DEFINED IN THE PROGRAM are
//!   compared, which is what keeps the pass sound without a module graph.
//!
//! The residual false-positive class is the one `unbound_symbol` already
//! documents and for the same reason: a binding form that is itself a
//! user macro (`(dolist (entry xs) …)`) is not syntactically
//! recognizable, so a name it binds is not seen as shadowed. Both
//! dissolve at the same place — running over macro-EXPANDED forms.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tatara_lisp::binding_shapes::{
    is_lambda_list_keyword, DEF_PREFIX, LAMBDA_HEADS, LET_HEADS, QUOTE_HEADS,
};
use tatara_lisp::{Atom, Span, Spanned, SpannedForm};

/// Static type known at build time. Mirrors the runtime type
/// vocabulary in `type_check.rs` but as a Rust enum (instead of a
/// Lisp Value tree) for cheap pattern matching.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StaticType {
    Any,
    Nil,
    Bool,
    Int,
    Float,
    Number,
    Str,
    Symbol,
    Keyword,
    List(Box<StaticType>),
    Map(Box<StaticType>, Box<StaticType>),
    Procedure,
    Promise,
    Error,
    /// Disjunction of branches — value matches if it matches any.
    Union(Vec<StaticType>),
    /// Function arrow: parameter types plus return type.
    ///
    /// SYNTHESIZED, never parsed. `from_spanned` keeps collapsing the
    /// `(:fn (T…) -> R)` ANNOTATION to [`Self::Procedure`] — the runtime
    /// `matches_type` checks nothing but callability for that spec, and
    /// widening the annotation would make the two disagree. This variant
    /// comes from a definition's own parameter list instead
    /// (`(define (f a b) …)` → two `Any` params, `Any` return), which is
    /// why it works on source carrying no annotations whatsoever.
    ///
    /// An arrow and a `Procedure` describe the same runtime value, so
    /// they conform to each other in both directions; `params` exists to
    /// be COUNTED, and in phase 1 every entry is `Any`.
    Fn {
        params: Vec<StaticType>,
        ret: Box<StaticType>,
    },
}

impl StaticType {
    /// Render as a tatara-lisp type-spec source string, matching
    /// `type_check::render_type` for end-to-end consistency.
    pub fn render(&self) -> String {
        match self {
            Self::Any => ":any".into(),
            Self::Nil => ":nil".into(),
            Self::Bool => ":bool".into(),
            Self::Int => ":int".into(),
            Self::Float => ":float".into(),
            Self::Number => ":number".into(),
            Self::Str => ":string".into(),
            Self::Symbol => ":symbol".into(),
            Self::Keyword => ":keyword".into(),
            Self::List(t) => format!("(:list-of {})", t.render()),
            Self::Map(k, v) => format!("(:map-of {} {})", k.render(), v.render()),
            Self::Procedure => ":procedure".into(),
            Self::Promise => ":promise".into(),
            Self::Error => ":error".into(),
            Self::Union(branches) => {
                let parts: Vec<String> = branches.iter().map(Self::render).collect();
                format!("(:union {})", parts.join(" "))
            }
            // `(:fn (T1 T2 …) -> R)` — the same surface syntax
            // `type_check`'s grammar documents and `render_type`
            // produces, so a diagnostic reads back as source.
            Self::Fn { params, ret } => {
                let parts: Vec<String> = params.iter().map(Self::render).collect();
                format!("(:fn ({}) -> {})", parts.join(" "), ret.render())
            }
        }
    }

    /// Does `self` (the inferred type) conform to `expected`? `Any`
    /// is the bottom-of-lattice escape hatch — both directions match.
    /// `Number` is the union of `Int` + `Float`. Everything else is
    /// strict structural equality.
    pub fn conforms_to(&self, expected: &StaticType) -> bool {
        if matches!(self, Self::Any) || matches!(expected, Self::Any) {
            return true;
        }
        if matches!(expected, Self::Number) && matches!(self, Self::Int | Self::Float) {
            return true;
        }
        if matches!(self, Self::Number) && matches!(expected, Self::Int | Self::Float) {
            // Could be either at runtime — concede.
            return true;
        }
        if let Self::Union(branches) = expected {
            return branches.iter().any(|b| self.conforms_to(b));
        }
        if let Self::Union(branches) = self {
            return branches.iter().all(|b| b.conforms_to(expected));
        }
        // A synthesized arrow and the erased `:procedure` / `(:fn …)`
        // annotation name the SAME runtime value — the annotation
        // surface collapses to `Procedure` (see `from_spanned`), so
        // without this an existing `(the :procedure f)` would start
        // failing the moment `f`'s definition began synthesizing an
        // arrow. Both directions, because either side can be the
        // erased one.
        if matches!(self, Self::Fn { .. }) && matches!(expected, Self::Procedure)
            || matches!(self, Self::Procedure) && matches!(expected, Self::Fn { .. })
        {
            return true;
        }
        match (self, expected) {
            (Self::List(a), Self::List(b)) => a.conforms_to(b),
            (Self::Map(ak, av), Self::Map(bk, bv)) => ak.conforms_to(bk) && av.conforms_to(bv),
            (
                Self::Fn {
                    params: ap,
                    ret: ar,
                },
                Self::Fn {
                    params: bp,
                    ret: br,
                },
            ) => {
                ap.len() == bp.len()
                    && ap.iter().zip(bp).all(|(a, b)| a.conforms_to(b))
                    && ar.conforms_to(br)
            }
            _ => self == expected,
        }
    }

    /// Parse a type spec from a Lisp source form (the same surface
    /// `type_check::matches_type` accepts at runtime). Returns `None`
    /// if the form is malformed — the caller emits a diagnostic.
    pub fn from_spanned(form: &Spanned) -> Option<Self> {
        match &form.form {
            SpannedForm::Atom(Atom::Keyword(k)) => Some(match k.as_str() {
                "any" => Self::Any,
                "nil" => Self::Nil,
                "bool" => Self::Bool,
                "int" => Self::Int,
                "float" => Self::Float,
                "number" => Self::Number,
                "string" => Self::Str,
                "symbol" => Self::Symbol,
                "keyword" => Self::Keyword,
                "procedure" | "fn" => Self::Procedure,
                "promise" => Self::Promise,
                "error" => Self::Error,
                "list" => Self::List(Box::new(Self::Any)),
                "map" => Self::Map(Box::new(Self::Any), Box::new(Self::Any)),
                _ => return None,
            }),
            SpannedForm::List(items) if !items.is_empty() => {
                let head = items[0].as_keyword()?;
                match head {
                    "list-of" if items.len() == 2 => {
                        Some(Self::List(Box::new(Self::from_spanned(&items[1])?)))
                    }
                    "map-of" if items.len() == 3 => Some(Self::Map(
                        Box::new(Self::from_spanned(&items[1])?),
                        Box::new(Self::from_spanned(&items[2])?),
                    )),
                    "union" => {
                        let mut branches = Vec::with_capacity(items.len() - 1);
                        for it in &items[1..] {
                            branches.push(Self::from_spanned(it)?);
                        }
                        Some(Self::Union(branches))
                    }
                    "fn" => Some(Self::Procedure),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// One static-type diagnostic. Includes everything the caller needs
/// to render a proper IDE squiggle.
#[derive(Debug, Clone)]
pub struct TypeDiagnostic {
    pub span: Span,
    pub kind: TypeDiagnosticKind,
}

#[derive(Debug, Clone)]
pub enum TypeDiagnosticKind {
    /// Inferred type doesn't match the declared expectation.
    Mismatch {
        expected: StaticType,
        got: StaticType,
        context: String,
    },
    /// Type spec was syntactically malformed (unknown keyword,
    /// wrong arity to (:list-of T) etc.).
    BadTypeSpec(String),
    /// A call passes the wrong NUMBER of arguments to a function whose
    /// definition is visible in this program. Counts, not types — see
    /// the module docs for why this is the diagnostic that survives an
    /// unannotated corpus.
    Arity {
        expected: usize,
        got: usize,
        context: String,
    },
}

impl TypeDiagnostic {
    pub fn render(&self, src: &str) -> String {
        let (line, col) = Span::line_col(src, self.span.start);
        let head = format!("type:{}", line);
        match &self.kind {
            TypeDiagnosticKind::Mismatch {
                expected,
                got,
                context,
            } => format!(
                "{head}:{col}: type mismatch in {context}: expected {}, got {}",
                expected.render(),
                got.render()
            ),
            TypeDiagnosticKind::BadTypeSpec(msg) => {
                format!("{head}:{col}: bad type spec — {msg}")
            }
            TypeDiagnosticKind::Arity {
                expected,
                got,
                context,
            } => format!("{head}:{col}: arity mismatch in {context}: expected {expected} argument(s), got {got}"),
        }
    }
}

/// Walk a parsed program, collecting type diagnostics. The pass is
/// PURE — it never evaluates anything, only inspects spans + shape.
///
/// Two passes over the top level. Pass 1 records every top-level
/// definition's SIGNATURE, so a call that textually precedes its
/// definition — or a pair of mutually recursive functions — is still
/// arity-checked; `unbound_symbol` collects definitions up front for
/// exactly the same reason. Pass 2 is the checking walk, which rebinds
/// each definition as it reaches it, so a redefinition later in the
/// file governs the calls that follow it.
pub fn check_program(forms: &[Spanned]) -> Vec<TypeDiagnostic> {
    let mut env = TypeEnv::default();
    for form in forms {
        if let SpannedForm::List(items) = &form.form {
            if let Some((name, arrow)) = definition_signature(items) {
                env.define(name, arrow);
            }
        }
    }
    let mut diags = Vec::new();
    for form in forms {
        check_form(form, &mut env, &mut diags);
    }
    diags
}

#[derive(Default)]
struct TypeEnv {
    /// Bindings inferred or declared at the top level. Used for
    /// looking up symbols when checking calls / references.
    bindings: HashMap<Arc<str>, StaticType>,
    /// Names bound by an ENCLOSING binder — lambda parameters, `let`
    /// names, `define`/`def…` parameters. A shadowed name's static type
    /// is unknown, so `lookup` must answer `Any` for it rather than
    /// handing back the top-level binding of the same name. Without
    /// this, the higher-order shape `(define (twice f x) (f (f x)))`
    /// would be arity-checked against an unrelated top-level `f`.
    scopes: Vec<HashSet<Arc<str>>>,
}

impl TypeEnv {
    fn lookup(&self, name: &str) -> StaticType {
        if self.scopes.iter().any(|s| s.contains(name)) {
            return StaticType::Any;
        }
        self.bindings.get(name).cloned().unwrap_or(StaticType::Any)
    }

    fn define(&mut self, name: impl Into<Arc<str>>, ty: StaticType) {
        self.bindings.insert(name.into(), ty);
    }

    fn push_scope(&mut self, names: impl IntoIterator<Item = Arc<str>>) {
        self.scopes.push(names.into_iter().collect());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
}

/// Check a single top-level form. Special forms with annotations
/// (`the`, `defn-typed`-expanded `define`, `declare`) drive the
/// check; everything else is just type-inferred for the purpose of
/// downstream lookups.
fn check_form(form: &Spanned, env: &mut TypeEnv, diags: &mut Vec<TypeDiagnostic>) {
    let SpannedForm::List(items) = &form.form else {
        return;
    };
    if let Some(head) = items.first().and_then(Spanned::as_symbol) {
        match head {
            "the" if items.len() == 3 => {
                check_the(&items[1], &items[2], env, diags);
                return;
            }
            "declare" if items.len() == 3 => {
                check_declare(&items[1], &items[2], env, diags);
                return;
            }
            // `define` before the generic `def…` arm below — it starts
            // with the same prefix but its third item is a VALUE, not a
            // parameter list.
            "define" if items.len() >= 3 => {
                check_define(items, env, diags);
                return;
            }
            // Quoted contents are DATA. `'(f 1 2 3)` is a list that
            // happens to start with a symbol, not a call to `f`.
            h if QUOTE_HEADS.contains(&h) => return,
            h if LAMBDA_HEADS.contains(&h) => {
                check_lambda(items, env, diags);
                return;
            }
            h if LET_HEADS.contains(&h) => {
                check_let(items, env, diags);
                return;
            }
            h if h.starts_with(DEF_PREFIX) => {
                check_def_family(items, env, diags);
                return;
            }
            _ => {}
        }
    }
    // Ordinary application — or a form this pass has no special
    // knowledge of, where treating the head as a call is exactly what
    // makes a wrong argument count detectable.
    check_application_arity(items, env, diags);
    // Recurse into children to surface nested annotations and calls.
    for item in items {
        check_form(item, env, diags);
    }
}

/// Compare a call's argument count against the definition's, when the
/// head resolves to a synthesized arrow. Anything else — a primitive, an
/// import, a macro, a shadowed local — is `Any` and abstains.
fn check_application_arity(items: &[Spanned], env: &TypeEnv, diags: &mut Vec<TypeDiagnostic>) {
    let Some(head) = items[0].as_symbol() else {
        return;
    };
    let StaticType::Fn { params, .. } = env.lookup(head) else {
        return;
    };
    let got = items.len() - 1;
    if got != params.len() {
        diags.push(TypeDiagnostic {
            span: items[0].span,
            kind: TypeDiagnosticKind::Arity {
                expected: params.len(),
                got,
                context: format!("call to {head}"),
            },
        });
    }
}

/// `(lambda (a b) body…)` / `(lambda rest body…)`, plus the `fn` / `λ`
/// aliases and `catch`, whose `(e)` binds identically.
fn check_lambda(items: &[Spanned], env: &mut TypeEnv, diags: &mut Vec<TypeDiagnostic>) {
    env.push_scope(param_names(items.get(1)));
    for item in items.iter().skip(2) {
        check_form(item, env, diags);
    }
    env.pop_scope();
}

/// `(let ((a 1) (b 2)) body…)` and its `let*` / `letrec` siblings.
///
/// All three are treated as if every name were visible everywhere in the
/// form — `letrec`'s rule. The three genuinely differ about whether an
/// initialiser sees a name, but for arity the over-wide scope can only
/// SUPPRESS a report (a name is treated as unknown-local when it might
/// still be the top-level function), never invent one, so the loose
/// approximation is the safe direction here. `unbound_symbol`, whose
/// reports go the other way, must and does distinguish them.
fn check_let(items: &[Spanned], env: &mut TypeEnv, diags: &mut Vec<TypeDiagnostic>) {
    let bindings = items.get(1);
    env.push_scope(let_binding_names(bindings));
    if let Some(SpannedForm::List(pairs)) = bindings.map(|b| &b.form) {
        for pair in pairs {
            if let SpannedForm::List(kv) = &pair.form {
                for init in kv.iter().skip(1) {
                    check_form(init, env, diags);
                }
            }
        }
    }
    for item in items.iter().skip(2) {
        check_form(item, env, diags);
    }
    env.pop_scope();
}

/// Every `def…` head that is not `define`: `defmacro` plus the open set
/// of driver and user-macro definition forms. Both spellings bind —
/// `(defmacro NAME (params…) body…)` and `(defthing (NAME params…)
/// body…)` — so both are scoped before the body is walked.
fn check_def_family(items: &[Spanned], env: &mut TypeEnv, diags: &mut Vec<TypeDiagnostic>) {
    let mut inner: Vec<Arc<str>> = Vec::new();
    if let Some(SpannedForm::List(sig)) = items.get(1).map(|t| &t.form) {
        inner.extend(symbol_names(sig).into_iter().skip(1));
    }
    // A list of ONLY symbols directly after the name is a parameter
    // list; anything else is body. When in doubt this treats the list as
    // params, which can only suppress a report, never invent one.
    let mut body_start = 2;
    if let Some(SpannedForm::List(sig)) = items.get(2).map(|p| &p.form) {
        let names = symbol_names(sig);
        if !sig.is_empty() && names.len() == sig.len() {
            inner.extend(names);
            body_start = 3;
        }
    }
    env.push_scope(inner);
    for item in items.iter().skip(body_start) {
        check_form(item, env, diags);
    }
    env.pop_scope();
}

fn check_the(
    type_form: &Spanned,
    expr: &Spanned,
    env: &mut TypeEnv,
    diags: &mut Vec<TypeDiagnostic>,
) {
    let Some(expected) = StaticType::from_spanned(type_form) else {
        diags.push(TypeDiagnostic {
            span: type_form.span,
            kind: TypeDiagnosticKind::BadTypeSpec(format!(
                "unrecognized type spec: {}",
                render_form_brief(type_form)
            )),
        });
        return;
    };
    let got = infer(expr, env);
    if !got.conforms_to(&expected) {
        diags.push(TypeDiagnostic {
            span: expr.span,
            kind: TypeDiagnosticKind::Mismatch {
                expected,
                got,
                context: "the-form".into(),
            },
        });
    }
    // Recurse into the expression to surface inner annotations too.
    check_form(expr, env, diags);
}

fn check_declare(
    name_form: &Spanned,
    type_form: &Spanned,
    env: &mut TypeEnv,
    diags: &mut Vec<TypeDiagnostic>,
) {
    let Some(name) = name_form.as_symbol() else {
        diags.push(TypeDiagnostic {
            span: name_form.span,
            kind: TypeDiagnosticKind::BadTypeSpec("declare: name must be a symbol".into()),
        });
        return;
    };
    let Some(ty) = StaticType::from_spanned(type_form) else {
        diags.push(TypeDiagnostic {
            span: type_form.span,
            kind: TypeDiagnosticKind::BadTypeSpec(format!(
                "unrecognized type spec: {}",
                render_form_brief(type_form)
            )),
        });
        return;
    };
    env.define(name, ty);
}

fn check_define(items: &[Spanned], env: &mut TypeEnv, diags: &mut Vec<TypeDiagnostic>) {
    // (define name expr) | (define (name args...) body...)
    match &items[1].form {
        SpannedForm::Atom(Atom::Symbol(name)) => {
            let expected = env.lookup(name).clone();
            let got = infer(&items[2], env);
            if !got.conforms_to(&expected) {
                diags.push(TypeDiagnostic {
                    span: items[2].span,
                    kind: TypeDiagnosticKind::Mismatch {
                        expected,
                        got: got.clone(),
                        context: format!("define {name}"),
                    },
                });
            }
            env.define(name.as_str(), got);
            check_form(&items[2], env, diags);
        }
        SpannedForm::List(sig) if !sig.is_empty() => {
            // (define (name params...) body...) — name binds to the
            // ARROW its own parameter list states, so call sites can be
            // counted. Params go into a fresh scope for the body walk,
            // because a parameter shadows any top-level function of the
            // same name.
            if let Some(name) = sig[0].as_symbol() {
                env.define(name, signature_arrow(&sig[1..]));
            }
            env.push_scope(symbol_names(&sig[1..]));
            for body_form in &items[2..] {
                check_form(body_form, env, diags);
            }
            env.pop_scope();
        }
        _ => {}
    }
}

/// The arrow a parameter list states, or plain `Procedure` when it
/// states no arity.
///
/// A lambda-list keyword (`&rest`, `&optional`, …) makes the signature
/// VARIADIC: counting the symbols before it would claim an arity the
/// function does not have, and every extra-argument call would report.
/// A non-symbol entry means the shape is not one this pass understands.
/// Both abstain by erasing to `Procedure`, which no call site checks.
fn signature_arrow(params: &[Spanned]) -> StaticType {
    let mut count = 0usize;
    for p in params {
        match p.as_symbol() {
            Some(name) if !is_lambda_list_keyword(name) => count += 1,
            _ => return StaticType::Procedure,
        }
    }
    StaticType::Fn {
        params: vec![StaticType::Any; count],
        ret: Box::new(StaticType::Any),
    }
}

/// The `(name, arrow)` a top-level form declares, for `check_program`'s
/// first pass. Covers both spellings of a function definition:
/// `(define (f a b) …)` and `(define f (lambda (a b) …))`.
fn definition_signature(items: &[Spanned]) -> Option<(Arc<str>, StaticType)> {
    if items.first().and_then(Spanned::as_symbol) != Some("define") || items.len() < 3 {
        return None;
    }
    match &items[1].form {
        SpannedForm::List(sig) if !sig.is_empty() => {
            let name = sig[0].as_symbol()?;
            Some((Arc::from(name), signature_arrow(&sig[1..])))
        }
        SpannedForm::Atom(Atom::Symbol(name)) => match lambda_arrow(&items[2]) {
            StaticType::Any => None,
            arrow => Some((Arc::from(name.as_str()), arrow)),
        },
        _ => None,
    }
}

/// The arrow a `(lambda (a b) …)` expression states, or `Any` when the
/// form is not lambda-shaped at all.
fn lambda_arrow(form: &Spanned) -> StaticType {
    let SpannedForm::List(items) = &form.form else {
        return StaticType::Any;
    };
    let Some(head) = items.first().and_then(Spanned::as_symbol) else {
        return StaticType::Any;
    };
    if !LAMBDA_HEADS.contains(&head) {
        return StaticType::Any;
    }
    match items.get(1).map(|p| &p.form) {
        Some(SpannedForm::List(sig)) => signature_arrow(sig),
        // `(lambda rest body)` — variadic with a single rest name.
        Some(SpannedForm::Atom(Atom::Symbol(_))) => StaticType::Procedure,
        // `(lambda () body)` reads as an empty list, i.e. `Nil`.
        Some(SpannedForm::Nil) => StaticType::Fn {
            params: Vec::new(),
            ret: Box::new(StaticType::Any),
        },
        _ => StaticType::Any,
    }
}

/// Every plain symbol in a flat list — parameter lists, `define`
/// signatures. Non-symbols are skipped rather than erroring; this feeds
/// SHADOWING, where a missed name can only cost a report, never add one.
fn symbol_names(items: &[Spanned]) -> Vec<Arc<str>> {
    items
        .iter()
        .filter_map(|i| match &i.form {
            SpannedForm::Atom(Atom::Symbol(s)) => Some(Arc::from(s.as_str())),
            _ => None,
        })
        .collect()
}

/// A lambda's parameter names: a list of symbols, or a bare rest symbol.
fn param_names(form: Option<&Spanned>) -> Vec<Arc<str>> {
    match form.map(|p| &p.form) {
        Some(SpannedForm::List(sig)) => symbol_names(sig),
        Some(SpannedForm::Atom(Atom::Symbol(s))) => vec![Arc::from(s.as_str())],
        _ => Vec::new(),
    }
}

/// `let`-family binding names from `((name init) …)`, tolerating a bare
/// `(name)` or a bare `name`.
fn let_binding_names(form: Option<&Spanned>) -> Vec<Arc<str>> {
    let Some(SpannedForm::List(bindings)) = form.map(|b| &b.form) else {
        return Vec::new();
    };
    bindings
        .iter()
        .filter_map(|b| match &b.form {
            SpannedForm::List(pair) => match pair.first().map(|p| &p.form) {
                Some(SpannedForm::Atom(Atom::Symbol(s))) => Some(Arc::from(s.as_str())),
                _ => None,
            },
            SpannedForm::Atom(Atom::Symbol(s)) => Some(Arc::from(s.as_str())),
            _ => None,
        })
        .collect()
}

/// Pure inference — returns the static type of an expression with
/// no side effects. Falls through to `Any` for anything we can't
/// statically determine.
fn infer(form: &Spanned, env: &TypeEnv) -> StaticType {
    match &form.form {
        SpannedForm::Nil => StaticType::Nil,
        SpannedForm::Atom(a) => match a {
            Atom::Bool(_) => StaticType::Bool,
            Atom::Int(_) => StaticType::Int,
            Atom::Float(_) => StaticType::Float,
            Atom::Str(_) => StaticType::Str,
            Atom::Keyword(_) => StaticType::Keyword,
            Atom::Symbol(s) => env.lookup(s),
        },
        SpannedForm::List(items) if !items.is_empty() => {
            // (the type expr) — inference yields the declared type.
            if let Some(head) = items[0].as_symbol() {
                if head == "the" && items.len() == 3 {
                    return StaticType::from_spanned(&items[1]).unwrap_or(StaticType::Any);
                }
                if head == "quote" {
                    return infer_quoted(&items[1]);
                }
                if head == "list" {
                    return infer_list_ctor(&items[1..], env);
                }
                // A lambda expression IS its arrow, so
                // `(define f (lambda (a b) …))` binds an arity through
                // `check_define`'s value path with no extra case there.
                if LAMBDA_HEADS.contains(&head) {
                    let arrow = lambda_arrow(form);
                    if !matches!(arrow, StaticType::Any) {
                        return arrow;
                    }
                }
                // A call to a function this program defines yields the
                // arrow's return type. Checked BEFORE the primitive
                // table so a user definition shadowing a builtin name
                // wins, which is what the interpreter does too.
                if let StaticType::Fn { ret, .. } = env.lookup(head) {
                    return *ret;
                }
                // Built-in primitive applications — small signature table.
                if let Some(t) = primitive_return_type(head) {
                    return t;
                }
            }
            StaticType::Any
        }
        SpannedForm::Quote(inner) => infer_quoted(inner),
        SpannedForm::Quasiquote(_) | SpannedForm::Unquote(_) | SpannedForm::UnquoteSplice(_) => {
            StaticType::Any
        }
        _ => StaticType::Any,
    }
}

fn infer_quoted(form: &Spanned) -> StaticType {
    // Quoted forms produce structural Values mirroring the source
    // shape — atom keywords stay keywords, lists become lists, etc.
    match &form.form {
        SpannedForm::Atom(Atom::Symbol(_)) => StaticType::Symbol,
        SpannedForm::Atom(Atom::Keyword(_)) => StaticType::Keyword,
        SpannedForm::Atom(Atom::Str(_)) => StaticType::Str,
        SpannedForm::Atom(Atom::Int(_)) => StaticType::Int,
        SpannedForm::Atom(Atom::Float(_)) => StaticType::Float,
        SpannedForm::Atom(Atom::Bool(_)) => StaticType::Bool,
        SpannedForm::Nil => StaticType::Nil,
        SpannedForm::List(_) => StaticType::List(Box::new(StaticType::Any)),
        _ => StaticType::Any,
    }
}

fn infer_list_ctor(args: &[Spanned], env: &TypeEnv) -> StaticType {
    if args.is_empty() {
        return StaticType::List(Box::new(StaticType::Any));
    }
    let mut element = infer(&args[0], env);
    for arg in &args[1..] {
        let next = infer(arg, env);
        element = least_upper_bound(element, next);
        if matches!(element, StaticType::Any) {
            break;
        }
    }
    StaticType::List(Box::new(element))
}

/// Compute the LUB (least-upper-bound) of two static types — the most
/// specific type that covers both. Used by list-constructor inference.
fn least_upper_bound(a: StaticType, b: StaticType) -> StaticType {
    if a == b {
        return a;
    }
    if matches!(a, StaticType::Any) || matches!(b, StaticType::Any) {
        return StaticType::Any;
    }
    if matches!(
        (&a, &b),
        (StaticType::Int, StaticType::Float) | (StaticType::Float, StaticType::Int)
    ) {
        return StaticType::Number;
    }
    StaticType::Union(vec![a, b])
}

/// Hard-coded return-type signatures for built-in primitives. Used
/// only for inference — applications without an entry default to
/// `Any`. The list mirrors the most-used primitives in the embedded
/// stdlib; extending it is one entry per primitive.
fn primitive_return_type(name: &str) -> Option<StaticType> {
    Some(match name {
        // arithmetic — always numeric; refine to Int when all args were
        // Int (currently we can't peek args here cheaply, so promote
        // to :number which conservatively conforms to both).
        "+" | "-" | "*" | "/" | "abs" | "min" | "max" | "modulo" | "expt" | "sqrt" | "floor"
        | "ceiling" | "round" | "truncate" | "gcd" | "lcm" | "sin" | "cos" | "tan" | "log"
        | "exp" | "inc" | "dec" => StaticType::Number,

        // comparisons + predicates — bool.
        "=" | "<" | ">" | "<=" | ">=" | "not=" | "null?" | "pair?" | "list?" | "symbol?"
        | "string?" | "integer?" | "number?" | "boolean?" | "procedure?" | "foreign?" | "atom?"
        | "keyword?" | "even?" | "odd?" | "zero?" | "positive?" | "negative?" | "empty?"
        | "not-empty?" | "any?" | "every?" | "member?" | "is?" | "hash-map?"
        | "hash-map-empty?" | "hash-map-has?" | "chan?" | "chan-closed?" | "promise?"
        | "error?" => StaticType::Bool,

        // list-returning.
        "list" | "cons" | "reverse" | "append" | "take" | "drop" | "range" | "map" | "filter"
        | "remove" | "concat" | "distinct" | "flatten" | "zip" | "partition" | "scan-left"
        | "iterate" | "repeatedly" | "drain!" | "hash-map-keys" | "hash-map-values"
        | "hash-map-entries" | "read-all" => StaticType::List(Box::new(StaticType::Any)),

        // map-returning.
        "hash-map" | "hash-map-set" | "hash-map-remove" | "hash-map-merge" | "hash-map-update" => {
            StaticType::Map(Box::new(StaticType::Any), Box::new(StaticType::Any))
        }

        // string-returning.
        "string-append" | "string" | "pr-str" | "symbol->string" | "keyword->string"
        | "error-message" => StaticType::Str,

        // counts / lengths.
        "length" | "count-if" | "find-index" | "position" | "compare" | "string-length"
        | "hash-map-count" | "chan-len" => StaticType::Int,

        // keyword-tag-returning helpers (`keyword?` is already in the
        // bool group above; do not duplicate it here).
        "type-of" | "error-tag" => StaticType::Keyword,

        // fall-through — caller treats as Any.
        _ => return None,
    })
}

fn render_form_brief(form: &Spanned) -> String {
    match &form.form {
        SpannedForm::Atom(Atom::Symbol(s)) => s.to_string(),
        SpannedForm::Atom(Atom::Keyword(k)) => format!(":{k}"),
        SpannedForm::Atom(Atom::Str(s)) => format!("{s:?}"),
        SpannedForm::Atom(Atom::Int(n)) => n.to_string(),
        SpannedForm::Atom(Atom::Float(n)) => n.to_string(),
        SpannedForm::Atom(Atom::Bool(b)) => if *b { "#t" } else { "#f" }.into(),
        SpannedForm::Nil => "()".into(),
        SpannedForm::List(_) => "(...)".into(),
        _ => "?".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tatara_lisp::read_spanned;

    fn check(src: &str) -> Vec<TypeDiagnostic> {
        let forms = read_spanned(src).unwrap();
        check_program(&forms)
    }

    #[test]
    fn no_annotations_no_diagnostics() {
        assert!(check("(define x 42) (+ 1 2)").is_empty());
    }

    #[test]
    fn the_with_correct_atom_passes() {
        assert!(check("(the :int 42)").is_empty());
        assert!(check("(the :string \"hi\")").is_empty());
        assert!(check("(the :bool #t)").is_empty());
    }

    #[test]
    fn the_with_wrong_atom_flags() {
        let diags = check("(the :int \"oops\")");
        assert_eq!(diags.len(), 1);
        match &diags[0].kind {
            TypeDiagnosticKind::Mismatch { expected, got, .. } => {
                assert!(matches!(expected, StaticType::Int));
                assert!(matches!(got, StaticType::Str));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn declare_then_define_match_passes() {
        assert!(check("(declare counter :int) (define counter 0)").is_empty());
    }

    #[test]
    fn declare_then_define_mismatch_flags() {
        let diags = check("(declare counter :int) (define counter \"oops\")");
        assert_eq!(diags.len(), 1);
        match &diags[0].kind {
            TypeDiagnosticKind::Mismatch { expected, .. } => {
                assert!(matches!(expected, StaticType::Int));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn list_ctor_infers_homogeneous_element_type() {
        // (the (:list-of :int) (list 1 2 3)) — passes.
        assert!(check("(the (:list-of :int) (list 1 2 3))").is_empty());
    }

    #[test]
    fn list_ctor_heterogeneous_widens_to_any_or_union() {
        // Mixing int and string falls to a union — :string alone
        // wouldn't conform to (:list-of :int).
        let diags = check("(the (:list-of :int) (list 1 \"x\" 3))");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn bad_type_spec_diagnoses() {
        let diags = check("(the :nonsense 1)");
        assert_eq!(diags.len(), 1);
        assert!(matches!(diags[0].kind, TypeDiagnosticKind::BadTypeSpec(_)));
    }

    #[test]
    fn primitive_return_type_drives_inference() {
        // (string-append "a" "b") infers as :string; flagging when
        // declared as :int.
        let diags = check("(the :int (string-append \"a\" \"b\"))");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn arithmetic_returns_number_so_conforms_to_int_or_float() {
        // (+ 1 2) is :number; conforms to both :int and :float.
        assert!(check("(the :int (+ 1 2))").is_empty());
        assert!(check("(the :float (+ 1.0 2.0))").is_empty());
    }

    #[test]
    fn union_type_admits_any_branch() {
        assert!(check("(the (:union :int :string) 42)").is_empty());
        assert!(check("(the (:union :int :string) \"hi\")").is_empty());
        let diags = check("(the (:union :int :string) #t)");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn nested_list_inference() {
        assert!(check("(the (:list-of (:list-of :int)) (list (list 1 2) (list 3)))").is_empty());
    }

    #[test]
    fn conforms_to_total_for_any() {
        assert!(StaticType::Any.conforms_to(&StaticType::Int));
        assert!(StaticType::Int.conforms_to(&StaticType::Any));
        assert!(
            StaticType::Union(vec![StaticType::Int, StaticType::Str]).conforms_to(&StaticType::Any)
        );
    }

    // ── arity ────────────────────────────────────────────────────
    //
    // Every case below is stated in BOTH directions. A one-directional
    // arity suite proves nothing useful: the pass is only worth running
    // if the wrong count reports AND the right count, on completely
    // unannotated source, stays silent. The negative half is the half
    // that can regress silently, so it outnumbers the positive half
    // here on purpose.

    fn arities(src: &str) -> Vec<(usize, usize)> {
        check(src)
            .into_iter()
            .filter_map(|d| match d.kind {
                TypeDiagnosticKind::Arity { expected, got, .. } => Some((expected, got)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn wrong_arity_call_is_flagged() {
        assert_eq!(
            arities("(define (f a b c) a) (f 1 2)"),
            vec![(3, 2)],
            "too few arguments must report"
        );
        assert_eq!(
            arities("(define (f a b c) a) (f 1 2 3 4)"),
            vec![(3, 4)],
            "too many arguments must report"
        );
        assert_eq!(
            arities("(define (f) 1) (f 1)"),
            vec![(0, 1)],
            "a zero-argument function called with one must report"
        );
    }

    #[test]
    fn correct_arity_call_with_unannotated_args_is_not_flagged() {
        // THE false-positive test. Not one type annotation in sight —
        // this is what the fleet's .tlisp actually looks like, and the
        // pass must be silent on it.
        assert!(arities("(define (f a b c) a) (f 1 \"two\" (list 3))").is_empty());
        assert!(arities("(define (greet name) name) (greet \"world\")").is_empty());
        assert!(arities("(define (nullary) 42) (nullary)").is_empty());
    }

    #[test]
    fn arity_diagnostic_renders_with_the_call_site_position() {
        let src = "(define (f a b) a)\n(f 1)";
        let diags = check(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].render(src),
            "type:2:2: arity mismatch in call to f: expected 2 argument(s), got 1"
        );
    }

    #[test]
    fn forward_and_mutually_recursive_calls_are_still_checked() {
        // The call precedes the definition — pass 1 is what makes this
        // report rather than silently infer `Any`.
        assert_eq!(
            arities("(define (g) (f 1)) (define (f a b) a)"),
            vec![(2, 1)]
        );
        // ...and a correct forward call stays silent.
        assert!(arities("(define (g) (f 1 2)) (define (f a b) a)").is_empty());
    }

    #[test]
    fn a_variadic_signature_claims_no_arity() {
        // `&rest` absorbs everything after it; counting `a` alone would
        // report on every 2+-argument call.
        assert!(arities("(define (f a &rest xs) a) (f 1)").is_empty());
        assert!(arities("(define (f a &rest xs) a) (f 1 2 3 4 5)").is_empty());
        // The fixed-arity sibling still reports, so the abstention above
        // is `&rest` doing its job rather than the check being dead.
        assert_eq!(arities("(define (f a xs) a) (f 1 2 3 4 5)"), vec![(2, 5)]);
    }

    #[test]
    fn a_parameter_shadows_a_top_level_function_of_the_same_name() {
        // The higher-order shape. `f` inside `twice` is the PARAMETER,
        // called with one argument; the top-level `f` takes two. Reading
        // through the shadow would flag correct code.
        assert!(arities("(define (f a b) a) (define (twice f x) (f (f x)))").is_empty());
        // A lambda parameter shadows too.
        assert!(arities("(define (f a b) a) (define (g) (lambda (f) (f 1)))").is_empty());
        // ...and a `let` name.
        assert!(arities("(define (f a b) a) (define (g h) (let ((f h)) (f 1)))").is_empty());
        // Un-shadowed, the same call reports — proving the three cases
        // above are shadowing and not a checker that stopped looking.
        assert_eq!(
            arities("(define (f a b) a) (define (g) (f 1))"),
            vec![(2, 1)]
        );
    }

    #[test]
    fn quoted_data_is_not_a_call() {
        assert!(arities("(define (f a b) a) (quote (f 1 2 3))").is_empty());
        assert!(arities("(define (f a b) a) '(f 1 2 3)").is_empty());
    }

    #[test]
    fn an_unknown_head_is_never_arity_checked() {
        // Primitives, imports and macros are all invisible to this pass;
        // it only counts against definitions it can see.
        assert!(arities("(string-append \"a\" \"b\" \"c\")").is_empty());
        assert!(arities("(some-macro a b c d e)").is_empty());
    }

    #[test]
    fn a_lambda_bound_by_name_carries_its_arity() {
        assert_eq!(arities("(define f (lambda (a b) a)) (f 1)"), vec![(2, 1)]);
        assert!(arities("(define f (lambda (a b) a)) (f 1 2)").is_empty());
    }

    #[test]
    fn nested_calls_inside_a_body_are_checked() {
        assert_eq!(
            arities("(define (f a b) a) (define (g x) (if x (f 1) (f 1 2)))"),
            vec![(2, 1)]
        );
    }

    #[test]
    fn arity_check_survives_a_defmacro_body() {
        // `defmacro`'s params must shadow; its template must not be read
        // as calls (it is a quasiquote, which this pass does not walk).
        assert!(arities("(define (f a b) a) (defmacro m (f x) `(,f ,x))").is_empty());
    }

    #[test]
    fn a_definition_infers_as_its_arrow_and_a_call_as_the_return_type() {
        let arrow = StaticType::Fn {
            params: vec![StaticType::Any, StaticType::Any],
            ret: Box::new(StaticType::Any),
        };
        assert_eq!(arrow.render(), "(:fn (:any :any) -> :any)");
        // An arrow and the erased `:procedure` annotation describe the
        // same value, both ways — so the pre-existing annotation path
        // keeps passing now that definitions synthesize arrows.
        assert!(arrow.conforms_to(&StaticType::Procedure));
        assert!(StaticType::Procedure.conforms_to(&arrow));
        assert!(check("(define (f a b) a) (the :procedure f)").is_empty());
        assert!(check("(define (f a b) a) (the (:fn (:int :int) -> :int) f)").is_empty());
    }

    #[test]
    fn arity_is_independent_of_inference_falling_to_any() {
        // The whole reason this pass exists: every argument here infers
        // `Any`, which conforms to everything — and the count still
        // catches the bug.
        let diags = check("(define (f a b) a) (f (some-unknown-thing) )");
        assert_eq!(diags.len(), 1);
        assert!(matches!(diags[0].kind, TypeDiagnosticKind::Arity { .. }));
    }

    #[test]
    fn render_round_trips_canonical_forms() {
        assert_eq!(StaticType::Int.render(), ":int");
        assert_eq!(
            StaticType::List(Box::new(StaticType::Str)).render(),
            "(:list-of :string)"
        );
        assert_eq!(
            StaticType::Map(Box::new(StaticType::Keyword), Box::new(StaticType::Int)).render(),
            "(:map-of :keyword :int)"
        );
        assert_eq!(
            StaticType::Union(vec![StaticType::Int, StaticType::Str]).render(),
            "(:union :int :string)"
        );
    }
}
