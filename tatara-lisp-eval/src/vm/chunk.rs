//! `Chunk` — a compiled program ready for the VM.
//!
//! A chunk owns its opcodes, its constant pool (literals too big for
//! embedded immediates), its name pool (interned strings used for
//! global / local names), and a function table (compiled lambda
//! bodies referenced by `MakeClosure`). Span lookup lets the VM map
//! a runtime error back to the source position.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tatara_lisp::{Span, Spanned};

use super::op::Op;
use crate::value::Value;

/// Which compiled function of a [`Chunk`]: its top level, or an entry of its
/// `fn_table`.
///
/// A VM frame records one so that a parked frame can be placed in a chunk
/// recompiled from the same source, where the function's `Arc` is a different
/// allocation but its index is the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FnRef {
    /// `chunk.top`, the program itself.
    Top,
    /// `chunk.fn_table[i]`, a lambda body.
    Table(usize),
}

impl FnRef {
    /// The function this names in `chunk`, or `None` when the chunk has no
    /// such entry.
    ///
    /// ```
    /// use tatara_lisp_eval::vm::{compile_program, FnRef};
    /// let chunk = compile_program(&tatara_lisp_eval::read_spanned("(lambda (x) x)").unwrap()).unwrap();
    /// assert!(FnRef::Top.resolve(&chunk).is_some());
    /// assert_eq!(FnRef::Table(0).resolve(&chunk).unwrap().params.len(), 1);
    /// assert!(FnRef::Table(1).resolve(&chunk).is_none());
    /// ```
    #[must_use]
    pub fn resolve(self, chunk: &Chunk) -> Option<&CompiledFn> {
        match self {
            Self::Top => Some(&chunk.top),
            Self::Table(i) => chunk.fn_table.get(i),
        }
    }
}

/// A BLAKE3 hash of what a [`Chunk`] executes, from [`Chunk::fingerprint`].
///
/// Two chunks compiled from the same source by the same compiler have the same
/// fingerprint; a chunk whose code, constants or names differ has another.
/// Spans are left out: they place diagnostics and do not change what runs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint(pub [u8; 32]);

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

fn canonical_const(v: &Value) -> String {
    match v {
        Value::Sexp(form, _) => format!("Sexp({form:?})"),
        Value::List(xs) => format!(
            "[{}]",
            xs.iter()
                .map(canonical_const)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => format!("{other:?}"),
    }
}

/// Where a captured variable comes from in the enclosing scope —
/// either a real local of the parent frame, or another captured slot
/// (when nested closures chain captures upward).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    /// Index into the parent function's locals.
    Local(usize),
    /// Index into the parent function's own captures array.
    Captured(usize),
}

/// One compiled function — either a top-level chunk (the program) or
/// a lambda body. Stored in the chunk's `fn_table` and referenced by
/// `MakeClosure(idx)`.
#[derive(Debug, Clone)]
pub struct CompiledFn {
    /// Param count (excluding rest).
    pub params: Vec<Arc<str>>,
    /// Optional rest-args param name.
    pub rest: Option<Arc<str>>,
    /// Number of locals reserved (params + lets).
    pub locals: usize,
    /// Free-variable descriptors. Each entry says where to pull the
    /// value from in the enclosing frame at MakeClosure time. Same
    /// order as `LoadCaptured(idx)` references in the body.
    pub captures: Vec<(Arc<str>, CaptureSource)>,
    /// Bytecode for this function.
    pub ops: Vec<Op>,
    /// Span of each instruction for error reporting. Same length as
    /// `ops`.
    pub spans: Vec<Span>,
    /// The function's source span (for `<closure>` debug output).
    pub source_span: Span,
    /// Original Spanned body forms (preserved by `compile_lambda` so
    /// a `Foreign(CompiledClosure)` can be lifted back to a
    /// tree-walker `Closure` when invoked through `Caller::apply_value`
    /// — the path native higher-order primitives take). Empty for the
    /// top-level CompiledFn (the program itself).
    pub source_body: Vec<tatara_lisp::Spanned>,
    /// The name `(define (name …) …)` gave it; `None` for a bare `lambda`
    /// and the top level. Read when a budget refusal names the function it
    /// fired in, so the VM and the tree-walker name the same one.
    pub name: Option<Arc<str>>,
}

impl Default for CompiledFn {
    fn default() -> Self {
        Self {
            params: Vec::new(),
            rest: None,
            locals: 0,
            captures: Vec::new(),
            ops: Vec::new(),
            spans: Vec::new(),
            source_span: Span::synthetic(),
            source_body: Vec::new(),
            name: None,
        }
    }
}

/// Constant pool for non-immediate Values that the program references.
/// Strings, lists, etc. — anything that doesn't fit a tagged-integer
/// or boolean opcode gets stashed here and indexed by `Op::Const(idx)`.
///
/// Deduplicates cheaply-comparable scalars (Symbol / Keyword / Str /
/// Float) at insertion time — repeat literals collapse to one entry.
/// More expensive comparisons (List, Map, Sexp) are left un-deduped:
/// detecting structural equality would be O(n) per push, and these
/// rarely repeat byte-identically anyway.
#[derive(Debug, Default, Clone)]
pub struct ConstPool {
    pub values: Vec<Value>,
}

impl ConstPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a constant + return its index. Dedupes scalar
    /// constants by Arc-pointer / value equality; otherwise appends.
    pub fn push(&mut self, v: Value) -> usize {
        if let Some(idx) = self.lookup_existing(&v) {
            return idx;
        }
        let idx = self.values.len();
        self.values.push(v);
        idx
    }

    /// Look up an existing equivalent constant. Restricted to scalar
    /// kinds where comparison is O(1) or O(name-length): Symbol /
    /// Keyword / Str compare by Arc-pointer first (covers the common
    /// case where the interner has already shared the Arc), falling
    /// back to byte equality. Float compares by bit-pattern (NaN
    /// dedupes consistently). Bool / Int / Nil are emitted via
    /// dedicated opcodes — they don't reach the const pool.
    fn lookup_existing(&self, v: &Value) -> Option<usize> {
        for (i, existing) in self.values.iter().enumerate() {
            if Self::scalar_equal(existing, v) {
                return Some(i);
            }
        }
        None
    }

    fn scalar_equal(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Symbol(x), Value::Symbol(y))
            | (Value::Keyword(x), Value::Keyword(y))
            | (Value::Str(x), Value::Str(y)) => std::sync::Arc::ptr_eq(x, y) || **x == **y,
            (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
            _ => false,
        }
    }

    pub fn get(&self, idx: usize) -> &Value {
        &self.values[idx]
    }
}

/// Name pool — `Arc<str>` interning keyed by the string itself.
/// Every `LoadGlobal` / `StoreGlobal` references a name by index here
/// so opcodes stay small and the lookup is O(1) instead of O(N).
#[derive(Debug, Default, Clone)]
pub struct NamePool {
    pub names: Vec<Arc<str>>,
    /// Index of a name in `names` if previously interned. We use a
    /// linear search because typical Lisp programs have <100 globals;
    /// upgrading to HashMap is trivial when needed.
    map: Vec<(Arc<str>, usize)>,
}

impl NamePool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, name: impl Into<Arc<str>>) -> usize {
        let name = name.into();
        for (existing, idx) in &self.map {
            if &**existing == &*name {
                return *idx;
            }
        }
        let idx = self.names.len();
        self.names.push(name.clone());
        self.map.push((name, idx));
        idx
    }

    pub fn get(&self, idx: usize) -> &Arc<str> {
        &self.names[idx]
    }
}

/// A compiled program — top-level + lambdas it references.
#[derive(Debug, Default, Clone)]
pub struct Chunk {
    /// Top-level function. Its body is the program; it has no params.
    pub top: CompiledFn,
    /// Lambdas referenced by `MakeClosure(idx)`.
    pub fn_table: Vec<CompiledFn>,
    /// Constant pool — strings, lists, sub-Sexps, etc.
    pub consts: ConstPool,
    /// Name pool — global symbol names + local-scope param names.
    pub names: NamePool,
}

impl Chunk {
    /// The fingerprint of what this chunk executes: every function's
    /// parameters, locals, captures, opcodes, name and source body, the
    /// constants and the names, with spans left out.
    ///
    /// [`crate::vm::Vm::from_parked`] compares it to refuse a parked state
    /// taken from different source rather than misread its indices.
    ///
    /// ```
    /// use tatara_lisp_eval::vm::compile_program;
    /// let compile = |src: &str| compile_program(&tatara_lisp_eval::read_spanned(src).unwrap()).unwrap();
    /// assert_eq!(compile("(+ 1 2)").fingerprint(), compile("(+  1   2)").fingerprint());
    /// assert_ne!(compile("(+ 1 2)").fingerprint(), compile("(+ 1 3)").fingerprint());
    /// ```
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let mut hasher = blake3::Hasher::new();
        let mut field = |text: &str| {
            hasher.update(&(text.len() as u64).to_le_bytes());
            hasher.update(text.as_bytes());
        };
        field(&self.fn_table.len().to_string());
        for func in std::iter::once(&self.top).chain(&self.fn_table) {
            field(&format!(
                "{:?}",
                (
                    &func.params,
                    &func.rest,
                    func.locals,
                    &func.captures,
                    &func.ops,
                    &func.name
                )
            ));
            field(&format!(
                "{:?}",
                func.source_body
                    .iter()
                    .map(Spanned::to_sexp)
                    .collect::<Vec<_>>()
            ));
        }
        field(&self.consts.values.len().to_string());
        for value in &self.consts.values {
            field(&canonical_const(value));
        }
        field(&format!("{:?}", self.names.names));
        Fingerprint(*hasher.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_pool_dedupes_repeat_strings() {
        let mut pool = ConstPool::new();
        let a = pool.push(Value::Str(Arc::from("hello")));
        let b = pool.push(Value::Str(Arc::from("hello")));
        let c = pool.push(Value::Str(Arc::from("world")));
        assert_eq!(a, b, "repeat string must dedupe");
        assert_ne!(a, c);
        assert_eq!(pool.values.len(), 2);
    }

    #[test]
    fn const_pool_dedupes_interned_symbols() {
        let mut pool = ConstPool::new();
        let s1 = crate::interner::intern("foo");
        let s2 = crate::interner::intern("foo");
        let a = pool.push(Value::Symbol(s1));
        let b = pool.push(Value::Symbol(s2));
        assert_eq!(a, b);
        assert_eq!(pool.values.len(), 1);
    }

    #[test]
    fn const_pool_dedupes_floats_by_bits() {
        let mut pool = ConstPool::new();
        let a = pool.push(Value::Float(3.14));
        let b = pool.push(Value::Float(3.14));
        let c = pool.push(Value::Float(2.71));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(pool.values.len(), 2);
    }

    #[test]
    fn const_pool_does_not_dedupe_lists() {
        // Structural equality on lists would be O(n) per push;
        // we explicitly skip it. This documents the trade-off.
        let mut pool = ConstPool::new();
        let _ = pool.push(Value::list(vec![Value::Int(1), Value::Int(2)]));
        let _ = pool.push(Value::list(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(pool.values.len(), 2, "lists deliberately not deduped");
    }

    #[test]
    fn name_pool_intern_returns_same_index() {
        let mut p = NamePool::new();
        let a = p.intern("foo");
        let b = p.intern("foo");
        assert_eq!(a, b);
        assert_eq!(p.names.len(), 1);
    }
}
