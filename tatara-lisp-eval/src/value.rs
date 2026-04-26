//! Runtime values.
//!
//! `Value` is distinct from `Sexp`: evaluation produces `Value`, while the
//! source AST is `Sexp` / `Spanned`. Values include runtime-only variants
//! (closures, native functions, opaque host-owned Foreign values) that
//! have no surface syntax.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use tatara_lisp::{Sexp, Span, Spanned};

use crate::env::Env;
use crate::ffi::Arity;

/// An evaluated runtime value.
#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    Symbol(Arc<str>),
    Keyword(Arc<str>),
    List(Arc<Vec<Value>>),
    Closure(Arc<Closure>),
    NativeFn(Arc<NativeFn>),
    /// A first-class structured error — Clojure ex-info shape:
    /// a tag (keyword/string), a message string, and a data plist.
    /// Constructed by `(error tag msg data)` / `(ex-info msg data)`.
    /// Raised by `(throw err)`. Caught by `(try ... (catch (e) ...))`.
    Error(Arc<ErrorObj>),
    /// Escape hatch: unevaluated source form carried as a value, e.g. after
    /// `(quote x)`. Preserves span info.
    Sexp(Sexp, Span),
    /// Opaque host-owned value. The embedder supplies these via FFI; native
    /// functions read them back via downcast. Used to expose typed Rust
    /// handles (job refs, client handles) to Lisp code.
    Foreign(Arc<dyn Any + Send + Sync>),
}

/// Structured error payload — tag + message + attached data. The data
/// is a list of (key, value) pairs preserving insertion order — a
/// plist-style alist. Keys are typically `Value::Keyword`s but any
/// equality-comparable Value works.
#[derive(Debug, Clone)]
pub struct ErrorObj {
    pub tag: Arc<str>,
    pub message: Arc<str>,
    pub data: Vec<(Value, Value)>,
}

/// A user-defined closure produced by `(lambda …)` or `(define (f …) …)`.
pub struct Closure {
    pub params: Vec<Arc<str>>,
    /// Optional rest parameter — `(lambda (a b . rest) …)` or
    /// `(lambda (a b &rest rs) …)`.
    pub rest: Option<Arc<str>>,
    /// Body forms, preserved as `Spanned` so error locations inside the
    /// body remain accurate after construction.
    pub body: Vec<Spanned>,
    pub captured_env: Env,
    pub source: Span,
}

/// A host-registered Rust function exposed to Lisp code. The actual
/// callable lives in the `Interpreter<H>`'s `FnRegistry`, keyed by
/// `name` — this struct carries just the lookup key and arity so
/// `Value` remains non-generic over `H`.
#[derive(Clone, Debug)]
pub struct NativeFn {
    pub name: Arc<str>,
    pub arity: Arity,
}

// ── Convenience constructors ────────────────────────────────────────────

impl Value {
    pub fn symbol(s: impl Into<Arc<str>>) -> Self {
        Self::Symbol(s.into())
    }

    pub fn keyword(s: impl Into<Arc<str>>) -> Self {
        Self::Keyword(s.into())
    }

    pub fn string(s: impl Into<Arc<str>>) -> Self {
        Self::Str(s.into())
    }

    pub fn list<I: IntoIterator<Item = Value>>(xs: I) -> Self {
        Self::List(Arc::new(xs.into_iter().collect()))
    }

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Self::Nil | Self::Bool(false))
    }

    /// Short type name for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Nil => "nil",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Str(_) => "string",
            Self::Symbol(_) => "symbol",
            Self::Keyword(_) => "keyword",
            Self::List(_) => "list",
            Self::Closure(_) => "closure",
            Self::NativeFn(_) => "native-fn",
            Self::Error(_) => "error",
            Self::Sexp(..) => "sexp",
            Self::Foreign(_) => "foreign",
        }
    }
}

// ── Debug / Display ─────────────────────────────────────────────────────

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("Nil"),
            Self::Bool(b) => write!(f, "Bool({b})"),
            Self::Int(n) => write!(f, "Int({n})"),
            Self::Float(n) => write!(f, "Float({n})"),
            Self::Str(s) => write!(f, "Str({s:?})"),
            Self::Symbol(s) => write!(f, "Symbol({s})"),
            Self::Keyword(s) => write!(f, "Keyword(:{s})"),
            Self::List(xs) => f.debug_list().entries(xs.iter()).finish(),
            Self::Closure(_) => f.write_str("Closure(…)"),
            Self::NativeFn(n) => write!(f, "NativeFn({})", n.name),
            Self::Error(e) => write!(f, "Error({}: {})", e.tag, e.message),
            Self::Sexp(s, sp) => write!(f, "Sexp({s} @ {sp})"),
            Self::Foreign(_) => f.write_str("Foreign(…)"),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("()"),
            Self::Bool(true) => f.write_str("#t"),
            Self::Bool(false) => f.write_str("#f"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(n) => write!(f, "{n}"),
            Self::Str(s) => write!(f, "{s:?}"),
            Self::Symbol(s) => f.write_str(s),
            Self::Keyword(s) => write!(f, ":{s}"),
            Self::List(xs) => {
                f.write_str("(")?;
                for (i, v) in xs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            Self::Closure(c) => {
                write!(f, "#<closure")?;
                if !c.params.is_empty() {
                    write!(f, " ({}", c.params.join(" "))?;
                    if let Some(rest) = &c.rest {
                        write!(f, " . {rest}")?;
                    }
                    write!(f, ")")?;
                }
                write!(f, ">")
            }
            Self::NativeFn(n) => write!(f, "#<native {}>", n.name),
            Self::Error(e) => {
                write!(f, "#<error :{} {:?}", e.tag, e.message.as_ref())?;
                if !e.data.is_empty() {
                    f.write_str(" {")?;
                    for (i, (k, v)) in e.data.iter().enumerate() {
                        if i > 0 {
                            f.write_str(" ")?;
                        }
                        write!(f, "{k} {v}")?;
                    }
                    f.write_str("}")?;
                }
                f.write_str(">")
            }
            Self::Sexp(s, _) => write!(f, "'{s}"),
            Self::Foreign(_) => f.write_str("#<foreign>"),
        }
    }
}

// ── Rust <-> Value conversions (partial; filled in Phase 2.4) ──────────

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Self::Int(n)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::Float(n)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::Str(Arc::from(s))
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::Str(Arc::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness() {
        assert!(Value::Bool(true).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Nil.is_truthy());
        assert!(Value::Int(0).is_truthy(), "zero is truthy (Scheme-ish)");
        assert!(Value::list(std::iter::empty::<Value>()).is_truthy());
    }

    #[test]
    fn display_primitives() {
        assert_eq!(Value::Int(42).to_string(), "42");
        assert_eq!(Value::Bool(true).to_string(), "#t");
        assert_eq!(Value::Bool(false).to_string(), "#f");
        assert_eq!(Value::symbol("foo").to_string(), "foo");
        assert_eq!(Value::keyword("k").to_string(), ":k");
        assert_eq!(Value::Nil.to_string(), "()");
    }

    #[test]
    fn display_list() {
        let v = Value::list([Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert_eq!(v.to_string(), "(1 2 3)");
    }

    #[test]
    fn type_names() {
        assert_eq!(Value::Int(0).type_name(), "int");
        assert_eq!(Value::Str(Arc::from("x")).type_name(), "string");
        assert_eq!(Value::Nil.type_name(), "nil");
    }
}
