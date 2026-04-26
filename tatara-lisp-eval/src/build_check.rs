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

use std::collections::HashMap;
use std::sync::Arc;

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
        match (self, expected) {
            (Self::List(a), Self::List(b)) => a.conforms_to(b),
            (Self::Map(ak, av), Self::Map(bk, bv)) => ak.conforms_to(bk) && av.conforms_to(bv),
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
        }
    }
}

/// Walk a parsed program, collecting type diagnostics. The pass is
/// PURE — it never evaluates anything, only inspects spans + shape.
pub fn check_program(forms: &[Spanned]) -> Vec<TypeDiagnostic> {
    let mut env = TypeEnv::default();
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
}

impl TypeEnv {
    fn lookup(&self, name: &str) -> StaticType {
        self.bindings
            .get(name)
            .cloned()
            .unwrap_or(StaticType::Any)
    }

    fn define(&mut self, name: impl Into<Arc<str>>, ty: StaticType) {
        self.bindings.insert(name.into(), ty);
    }
}

/// Check a single top-level form. Special forms with annotations
/// (`the`, `defn-typed`-expanded `define`, `declare`) drive the
/// check; everything else is just type-inferred for the purpose of
/// downstream lookups.
fn check_form(form: &Spanned, env: &mut TypeEnv, diags: &mut Vec<TypeDiagnostic>) {
    if let SpannedForm::List(items) = &form.form {
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
                "define" if items.len() >= 3 => {
                    check_define(items, env, diags);
                    return;
                }
                _ => {}
            }
        }
        // Recurse into children to surface nested annotations.
        for item in items {
            check_form(item, env, diags);
        }
    }
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
        SpannedForm::List(head) if !head.is_empty() => {
            // (define (name args...) body...) — name binds to a procedure.
            if let Some(name) = head[0].as_symbol() {
                env.define(name, StaticType::Procedure);
            }
            for body_form in &items[2..] {
                check_form(body_form, env, diags);
            }
        }
        _ => {}
    }
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
                // Built-in primitive applications — small signature table.
                if let Some(t) = primitive_return_type(head) {
                    return t;
                }
            }
            StaticType::Any
        }
        SpannedForm::Quote(inner) => infer_quoted(inner),
        SpannedForm::Quasiquote(_)
        | SpannedForm::Unquote(_)
        | SpannedForm::UnquoteSplice(_) => StaticType::Any,
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
    if matches!((&a, &b),
        (StaticType::Int, StaticType::Float)
            | (StaticType::Float, StaticType::Int))
    {
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
        | "string?" | "integer?" | "number?" | "boolean?" | "procedure?" | "foreign?"
        | "atom?" | "keyword?" | "even?" | "odd?" | "zero?" | "positive?" | "negative?"
        | "empty?" | "not-empty?" | "any?" | "every?" | "member?" | "is?"
        | "hash-map?" | "hash-map-empty?" | "hash-map-has?" | "chan?" | "chan-closed?"
        | "promise?" | "error?" => StaticType::Bool,

        // list-returning.
        "list" | "cons" | "reverse" | "append" | "take" | "drop" | "range" | "map"
        | "filter" | "remove" | "concat" | "distinct" | "flatten" | "zip" | "partition"
        | "scan-left" | "iterate" | "repeatedly" | "drain!" | "hash-map-keys"
        | "hash-map-values" | "hash-map-entries" | "read-all" => {
            StaticType::List(Box::new(StaticType::Any))
        }

        // map-returning.
        "hash-map" | "hash-map-set" | "hash-map-remove" | "hash-map-merge"
        | "hash-map-update" => StaticType::Map(Box::new(StaticType::Any), Box::new(StaticType::Any)),

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
        assert!(StaticType::Union(vec![StaticType::Int, StaticType::Str])
            .conforms_to(&StaticType::Any));
    }

    #[test]
    fn render_round_trips_canonical_forms() {
        assert_eq!(StaticType::Int.render(), ":int");
        assert_eq!(StaticType::List(Box::new(StaticType::Str)).render(), "(:list-of :string)");
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
