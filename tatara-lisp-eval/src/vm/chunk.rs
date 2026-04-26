//! `Chunk` — a compiled program ready for the VM.
//!
//! A chunk owns its opcodes, its constant pool (literals too big for
//! embedded immediates), its name pool (interned strings used for
//! global / local names), and a function table (compiled lambda
//! bodies referenced by `MakeClosure`). Span lookup lets the VM map
//! a runtime error back to the source position.

use std::sync::Arc;

use tatara_lisp::Span;

use super::op::Op;
use crate::value::Value;

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
        }
    }
}

/// Constant pool for non-immediate Values that the program references.
/// Strings, lists, etc. — anything that doesn't fit a tagged-integer
/// or boolean opcode gets stashed here and indexed by `Op::Const(idx)`.
#[derive(Debug, Default, Clone)]
pub struct ConstPool {
    pub values: Vec<Value>,
}

impl ConstPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a constant + return its index. Doesn't dedupe — the
    /// compiler can hand-dedupe via small caches if it cares; for now
    /// simplicity wins.
    pub fn push(&mut self, v: Value) -> usize {
        let idx = self.values.len();
        self.values.push(v);
        idx
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
