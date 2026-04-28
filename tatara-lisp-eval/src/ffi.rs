//! FFI — register Rust functions as callable Lisp procedures.
//!
//! Two registration modes:
//!
//!   - **Raw** (`Interpreter::register_fn`): you receive `&[Value]` and
//!     pick values out yourself. Most flexible, no marshalling overhead.
//!     Appropriate for primitives that need to inspect arg kinds
//!     directly or handle variadic arguments.
//!
//!   - **Typed** (`Interpreter::register_typed{0,1,2,3,4}`): you declare
//!     Rust arg + return types; the runtime marshals `Value` ↔ Rust
//!     types via the `FromValue` and `IntoValue` traits. Arity is
//!     inferred from the Rust signature. This is the common-case API
//!     for embedder code.
//!
//! Values that need to cross the FFI boundary unchanged (e.g., opaque
//! host handles) can be wrapped in `Value::Foreign(Arc<dyn Any>)` and
//! downcast in the native fn body.

use std::sync::Arc;

use tatara_lisp::Span;

use crate::error::{EvalError, Result};
use crate::value::Value;

/// How many arguments a registered function accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    Exact(usize),
    AtLeast(usize),
    Range(usize, usize),
    Any,
}

impl Arity {
    /// Check `got` against this arity; returns `Ok(())` or a reason string.
    pub fn check(&self, got: usize) -> std::result::Result<(), String> {
        match *self {
            Self::Exact(n) if got == n => Ok(()),
            Self::Exact(n) => Err(format!("expected exactly {n}, got {got}")),
            Self::AtLeast(n) if got >= n => Ok(()),
            Self::AtLeast(n) => Err(format!("expected at least {n}, got {got}")),
            Self::Range(lo, hi) if got >= lo && got <= hi => Ok(()),
            Self::Range(lo, hi) => Err(format!("expected {lo}..={hi}, got {got}")),
            Self::Any => Ok(()),
        }
    }
}

/// A native Rust function the host has registered. Parameterized over the
/// host context type `H` so the callable can read/write host state.
///
/// The simple flavor — no access to the function registry. Use this for
/// primitives that operate purely on `Value` arguments. For higher-order
/// primitives (`map`, `filter`, `fold`, ...) that need to invoke a
/// callable `Value`, register via `Interpreter::register_higher_order_fn`
/// instead — the host then receives a `Caller` it can use to call back
/// into the eval loop.
pub trait NativeCallable<H>: Send + Sync + 'static {
    fn call(&self, args: &[Value], host: &mut H, call_span: Span) -> Result<Value>;
}

impl<H, F> NativeCallable<H> for F
where
    F: Fn(&[Value], &mut H, Span) -> Result<Value> + Send + Sync + 'static,
{
    fn call(&self, args: &[Value], host: &mut H, call_span: Span) -> Result<Value> {
        (self)(args, host, call_span)
    }
}

/// A higher-order Rust primitive — receives a `Caller` so it can invoke
/// `Value::Closure` / `Value::NativeFn` arguments back into the eval loop.
/// Used by `map`, `filter`, `fold`, `for-each`, and friends.
pub trait HigherOrderCallable<H>: Send + Sync + 'static {
    fn call(
        &self,
        args: &[Value],
        host: &mut H,
        caller: &Caller<H>,
        call_span: Span,
    ) -> Result<Value>;
}

impl<H, F> HigherOrderCallable<H> for F
where
    F: Fn(&[Value], &mut H, &Caller<H>, Span) -> Result<Value> + Send + Sync + 'static,
{
    fn call(
        &self,
        args: &[Value],
        host: &mut H,
        caller: &Caller<H>,
        call_span: Span,
    ) -> Result<Value> {
        (self)(args, host, caller, call_span)
    }
}

/// Handle that a higher-order primitive uses to invoke a callable `Value`
/// back into the eval loop. Holds borrows of the eval-time read-only
/// state — the function registry and the macro expander. `apply_value`
/// dispatches through whichever `Value` kind the callee is (`Closure`,
/// `NativeFn`, `HigherOrderFn`).
///
/// Construction is private — `Caller` only ever appears via
/// `HigherOrderCallable::call`, so primitives can only obtain one for the
/// duration of the call they're servicing.
pub struct Caller<'a, H> {
    pub(crate) registry: &'a FnRegistry<H>,
    pub(crate) expander: &'a tatara_lisp::SpannedExpander,
}

impl<'a, H: 'static> Caller<'a, H> {
    /// Apply a callable `Value` to `args` against this caller's registry.
    /// Mirrors the eval loop's `apply` precisely — closures get a fresh
    /// frame; native fns dispatch through the registry; higher-order
    /// fns receive a fresh `Caller` of their own.
    pub fn apply_value(
        &self,
        callee: &Value,
        args: Vec<Value>,
        host: &mut H,
        call_span: Span,
    ) -> Result<Value> {
        crate::eval::apply_external(callee, args, call_span, self.registry, self.expander, host)
    }

    /// Borrow the macro expander — primitives like `macroexpand-1`
    /// look up registered macros through this handle.
    pub fn expander(&self) -> &tatara_lisp::SpannedExpander {
        self.expander
    }

    /// Convenience: call a unary callable with one arg. Errors with a
    /// canonical message if the callee is not a procedure.
    pub fn call1(&self, f: &Value, x: Value, host: &mut H, span: Span) -> Result<Value> {
        self.apply_value(f, vec![x], host, span)
    }

    /// Convenience: call a binary callable with two args.
    pub fn call2(&self, f: &Value, a: Value, b: Value, host: &mut H, span: Span) -> Result<Value> {
        self.apply_value(f, vec![a, b], host, span)
    }
}

/// One registered callable. Internal storage; primitives don't see this.
/// `Arc` (not `Box`) so the apply path can clone the callable out of the
/// registry borrow before invoking it — letting `apply()` hold `&mut
/// Interpreter` while a higher-order primitive runs (which lets that
/// primitive re-enter the dispatch path with the same Interpreter).
pub(crate) enum FnImpl<H> {
    Native(Arc<dyn NativeCallable<H>>),
    Higher(Arc<dyn HigherOrderCallable<H>>),
}

impl<H> Clone for FnImpl<H> {
    fn clone(&self) -> Self {
        match self {
            Self::Native(f) => Self::Native(Arc::clone(f)),
            Self::Higher(f) => Self::Higher(Arc::clone(f)),
        }
    }
}

/// Registry of registered native functions for an `Interpreter<H>`.
pub(crate) struct FnRegistry<H> {
    entries: Vec<FnEntry<H>>,
}

pub(crate) struct FnEntry<H> {
    pub name: Arc<str>,
    /// Kept for future registry introspection — arity checking at call
    /// time uses the copy on `Value::NativeFn` for a quicker path.
    #[allow(dead_code)]
    pub arity: Arity,
    pub callable: FnImpl<H>,
}

impl<H> Default for FnRegistry<H> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<H> FnRegistry<H> {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&mut self, entry: FnEntry<H>) {
        // Shadow any earlier registration with the same name — last wins.
        if let Some(slot) = self.entries.iter_mut().find(|e| e.name == entry.name) {
            *slot = entry;
        } else {
            self.entries.push(entry);
        }
    }

    pub(crate) fn lookup(&self, name: &str) -> Option<&FnEntry<H>> {
        self.entries.iter().find(|e| &*e.name == name)
    }
}

// ── Typed marshalling ──────────────────────────────────────────────────

/// Convert from a Lisp `Value` into a Rust value. Implemented for the
/// common primitive types and for `Value` itself (identity). Used by the
/// `register_typed{N}` helpers to destructure args.
pub trait FromValue: Sized {
    fn from_value(v: &Value, at: Span) -> Result<Self>;
}

impl FromValue for Value {
    fn from_value(v: &Value, _at: Span) -> Result<Self> {
        Ok(v.clone())
    }
}

impl FromValue for i64 {
    fn from_value(v: &Value, at: Span) -> Result<Self> {
        match v {
            Value::Int(n) => Ok(*n),
            other => Err(EvalError::type_mismatch("integer", other.type_name(), at)),
        }
    }
}

impl FromValue for f64 {
    fn from_value(v: &Value, at: Span) -> Result<Self> {
        match v {
            Value::Int(n) => Ok(*n as f64),
            Value::Float(n) => Ok(*n),
            other => Err(EvalError::type_mismatch("number", other.type_name(), at)),
        }
    }
}

impl FromValue for bool {
    fn from_value(v: &Value, at: Span) -> Result<Self> {
        match v {
            Value::Bool(b) => Ok(*b),
            other => Err(EvalError::type_mismatch("bool", other.type_name(), at)),
        }
    }
}

impl FromValue for String {
    fn from_value(v: &Value, at: Span) -> Result<Self> {
        match v {
            Value::Str(s) => Ok(s.to_string()),
            other => Err(EvalError::type_mismatch("string", other.type_name(), at)),
        }
    }
}

impl FromValue for Arc<str> {
    fn from_value(v: &Value, at: Span) -> Result<Self> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            Value::Symbol(s) => Ok(s.clone()),
            Value::Keyword(s) => Ok(s.clone()),
            other => Err(EvalError::type_mismatch(
                "string/symbol",
                other.type_name(),
                at,
            )),
        }
    }
}

impl FromValue for Vec<Value> {
    fn from_value(v: &Value, at: Span) -> Result<Self> {
        match v {
            Value::Nil => Ok(Vec::new()),
            Value::List(xs) => Ok(xs.as_ref().clone()),
            other => Err(EvalError::type_mismatch("list", other.type_name(), at)),
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: &Value, at: Span) -> Result<Self> {
        match v {
            Value::Nil => Ok(None),
            other => T::from_value(other, at).map(Some),
        }
    }
}

/// Convert a Rust value into a `Value` for Lisp. Implemented for the
/// primitive types. The blanket `From<T> for Value` impls cover most
/// cases; this trait is the named interface used by typed-helper
/// registration.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

impl IntoValue for () {
    fn into_value(self) -> Value {
        Value::Nil
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::Int(self)
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::Float(self)
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::Str(Arc::from(self))
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::Str(Arc::from(self))
    }
}

impl IntoValue for Arc<str> {
    fn into_value(self) -> Value {
        Value::Str(self)
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Value {
        match self {
            None => Value::Nil,
            Some(x) => x.into_value(),
        }
    }
}

impl<T: IntoValue> IntoValue for Vec<T> {
    fn into_value(self) -> Value {
        Value::list(self.into_iter().map(IntoValue::into_value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arity_check() {
        assert!(Arity::Exact(2).check(2).is_ok());
        assert!(Arity::Exact(2).check(3).is_err());
        assert!(Arity::AtLeast(1).check(5).is_ok());
        assert!(Arity::AtLeast(1).check(0).is_err());
        assert!(Arity::Range(1, 3).check(2).is_ok());
        assert!(Arity::Range(1, 3).check(4).is_err());
        assert!(Arity::Any.check(0).is_ok());
        assert!(Arity::Any.check(1000).is_ok());
    }

    #[test]
    fn from_value_round_trips_primitives() {
        let sp = Span::synthetic();
        assert_eq!(i64::from_value(&Value::Int(42), sp).unwrap(), 42);
        assert_eq!(f64::from_value(&Value::Float(1.5), sp).unwrap(), 1.5);
        assert!(bool::from_value(&Value::Bool(true), sp).unwrap());
        assert_eq!(
            String::from_value(&Value::Str(Arc::from("hi")), sp).unwrap(),
            "hi"
        );
    }

    #[test]
    fn from_value_int_to_float_coerces() {
        let sp = Span::synthetic();
        assert_eq!(f64::from_value(&Value::Int(3), sp).unwrap(), 3.0);
    }

    #[test]
    fn from_value_option_nil_is_none() {
        let sp = Span::synthetic();
        assert_eq!(
            <Option<i64> as FromValue>::from_value(&Value::Nil, sp).unwrap(),
            None
        );
        assert_eq!(
            <Option<i64> as FromValue>::from_value(&Value::Int(7), sp).unwrap(),
            Some(7)
        );
    }

    #[test]
    fn from_value_type_mismatch_reports_expected_kind() {
        let sp = Span::synthetic();
        let err = i64::from_value(&Value::Str(Arc::from("x")), sp).unwrap_err();
        assert!(matches!(
            err,
            EvalError::TypeMismatch {
                expected: "integer",
                ..
            }
        ));
    }

    #[test]
    fn into_value_round_trips() {
        assert!(matches!(42i64.into_value(), Value::Int(42)));
        assert!(matches!(true.into_value(), Value::Bool(true)));
        assert!(matches!(().into_value(), Value::Nil));
        match String::from("hello").into_value() {
            Value::Str(s) => assert_eq!(&*s, "hello"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn into_value_vec_produces_list() {
        let v: Vec<i64> = vec![1, 2, 3];
        match v.into_value() {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[0], Value::Int(1)));
            }
            other => panic!("{other:?}"),
        }
    }
}
