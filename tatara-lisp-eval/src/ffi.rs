//! FFI — register Rust functions as callable Lisp procedures.
//!
//! Phase 2.2 scaffold: public types only. `register_fn`, the typed-helper
//! macros, and value marshalling land in Phase 2.4.

use std::sync::Arc;

use tatara_lisp::Span;

use crate::error::Result;
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

/// Registry of registered native functions for an `Interpreter<H>`.
/// Phase 2.2: placeholder; real dispatch + typed helpers in Phase 2.4.
#[allow(dead_code)]
pub(crate) struct FnRegistry<H> {
    entries: Vec<FnEntry<H>>,
}

#[allow(dead_code)]
pub(crate) struct FnEntry<H> {
    pub name: Arc<str>,
    pub arity: Arity,
    pub callable: Box<dyn NativeCallable<H>>,
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

    #[allow(dead_code)]
    pub(crate) fn insert(&mut self, entry: FnEntry<H>) {
        // Shadow any earlier registration with the same name — last wins.
        if let Some(slot) = self.entries.iter_mut().find(|e| e.name == entry.name) {
            *slot = entry;
        } else {
            self.entries.push(entry);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn lookup(&self, name: &str) -> Option<&FnEntry<H>> {
        self.entries.iter().find(|e| &*e.name == name)
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
}
