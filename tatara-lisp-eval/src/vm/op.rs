//! Opcode set — minimum viable for Lisp expressivity.
//!
//! Every opcode is a single enum variant. The dispatch loop in
//! `run.rs` matches on these. Stack effects are documented per opcode
//! so the compiler emits balanced code; mismatches are caught by the
//! `assert_balanced` invariant in tests.

/// One bytecode instruction. The `usize` arguments are indices into
/// the chunk's const pool, the locals array, or absolute IP offsets
/// (for jumps). All offsets are absolute (within a chunk) so we don't
/// need to recompute relative offsets when patching.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // ── Literals + constants ──────────────────────────────────────
    /// `→ v` — push constant from `chunk.consts[idx]`.
    Const(usize),
    /// `→ v` — push embedded i64 immediate (small ints, common case).
    Int(i64),
    /// `→ Nil`.
    Nil,
    /// `→ #t`.
    True,
    /// `→ #f`.
    False,

    // ── Variables ────────────────────────────────────────────────
    /// `→ v` — push locals[idx] (relative to current frame's locals_base).
    LoadLocal(usize),
    /// `v →` — pop and store into locals[idx].
    StoreLocal(usize),
    /// `→ v` — push captures[idx] of the closure that produced this
    /// frame. The closure populated its captures array at MakeClosure
    /// time from the enclosing frame's locals + captures.
    LoadCaptured(usize),
    /// `v →` — pop and write into captures[idx]. Mutates the
    /// captured cell, so two closures sharing the same upvalue see
    /// each other's writes (Scheme `set!` over a let binding).
    StoreCaptured(usize),
    /// `→ v` — look up name in interpreter's global env.
    /// `idx` is into the chunk's `name_pool`.
    LoadGlobal(usize),
    /// `v →` — pop, define under name globals[name_pool[idx]].
    StoreGlobal(usize),

    // ── Stack manipulation ───────────────────────────────────────
    /// `v →` — drop top of stack.
    Pop,
    /// `v → v v` — duplicate top of stack.
    Dup,

    // ── Control flow ─────────────────────────────────────────────
    /// `→` — unconditional jump to absolute IP.
    Jmp(usize),
    /// `v →` — pop; jump if falsy.
    JmpNot(usize),
    /// `v →` — pop; jump if truthy.
    JmpIf(usize),

    // ── Calls ────────────────────────────────────────────────────
    /// `f a1 ... aN → r` — pop callable + N args; push the result.
    /// The callable is `arity + 1` deep at call time.
    Call(usize),
    /// `f a1 ... aN → r` — like Call but reuses the current frame
    /// for TCO. Only emit at tail position; the compiler tracks tail
    /// position structurally.
    TailCall(usize),
    /// `f a b → r` — `Call(2)` of a global operator. When `f` is still the
    /// interpreter's own primitive for `kind` and both operands are Ints or
    /// both Floats, the VM computes `r` in place; otherwise it is the call.
    Arith(Arith),
    /// `v →` — pop the return value, restore the previous frame,
    /// push v as the result of the outer call.
    Return,

    // ── Lambda / closure ─────────────────────────────────────────
    /// `→ closure` — instantiate a closure from chunk's `fn_table[idx]`,
    /// snapshotting the current env into `captured_env`.
    MakeClosure(usize),

    // ── Sequencing / construction ─────────────────────────────────
    /// `vN ... v1 → list` — pop N values, build a list (reverse-order
    /// pop so list reads in source order).
    MakeList(usize),

    // ── Termination ──────────────────────────────────────────────
    // ── Tree-walker fallback ──────────────────────────────────────
    /// Evaluate the const-pool entry at `idx` (which must be a
    /// `Value::Sexp`) through the host `Interpreter`'s tree-walker.
    /// Push the result. Used by the compiler for any form it doesn't
    /// natively support (quasi-quote, require, provide, delay, eval,
    /// macroexpand, etc.) so the VM is drop-in compatible with every
    /// program the tree-walker accepts.
    ///
    /// Limitation: the dispatched form runs against the Interpreter's
    /// globals env, NOT the VM's frame-local variables. Forms with
    /// free references to VM locals (e.g. quasi-quote inside a let)
    /// will see those references as unbound. For forms that don't
    /// reference locals (top-level quasi-quotes, require, provide,
    /// eval/macroexpand at top level) this is a clean drop-in.
    EvalSexp(usize),

    // ── Exceptions / try-catch ────────────────────────────────────
    /// Push an error handler onto the current frame's handler stack.
    /// `catch_ip` is the absolute IP to jump to on error. `error_local`
    /// is the local slot the runtime stores the error Value into
    /// before resuming at `catch_ip`. Stack effect: 0.
    PushHandler { catch_ip: usize, error_local: usize },
    /// Pop the most recent handler. Stack effect: 0.
    PopHandler,

    /// `→` — stop the program. The top-of-stack value becomes the
    /// program's result.
    Halt,
}

impl Op {
    /// Net stack effect: positive means it grows the stack, negative
    /// means it shrinks. Variadic ops return `None` (caller computes).
    /// Used by the compiler to assert balance + by tests.
    #[must_use]
    pub fn stack_effect(&self) -> Option<i32> {
        Some(match self {
            Self::Const(_) | Self::Int(_) | Self::Nil | Self::True | Self::False => 1,
            Self::LoadLocal(_) | Self::LoadGlobal(_) | Self::LoadCaptured(_) => 1,
            Self::StoreLocal(_) | Self::StoreGlobal(_) | Self::StoreCaptured(_) => -1,
            Self::Pop => -1,
            Self::Dup => 1,
            Self::Jmp(_) => 0,
            Self::JmpNot(_) | Self::JmpIf(_) => -1,
            Self::Call(arity) | Self::TailCall(arity) => -(*arity as i32),
            Self::Arith(_) => -2,
            Self::Return => -1,
            Self::MakeClosure(_) => 1,
            Self::MakeList(n) => 1 - *n as i32,
            Self::EvalSexp(_) => 1,
            Self::PushHandler { .. } | Self::PopHandler => 0,
            Self::Halt => 0,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Lt,
    Gt,
    Le,
    Ge,
    NumEq,
    Equal,
    NotEqual,
}

impl Arith {
    /// How many kinds there are: the size of every per-kind table.
    pub const COUNT: usize = 10;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Add,
        Self::Sub,
        Self::Mul,
        Self::Lt,
        Self::Gt,
        Self::Le,
        Self::Ge,
        Self::NumEq,
        Self::Equal,
        Self::NotEqual,
    ];

    /// The kinds the standard library's own primitives answer, adopted when
    /// it is installed. [`Self::Equal`] and [`Self::NotEqual`] name functions
    /// an embedder defines; they run in place only after the embedder adopts
    /// its own (`Interpreter::adopt_operator`).
    pub const CORE: [Self; 8] = [
        Self::Add,
        Self::Sub,
        Self::Mul,
        Self::Lt,
        Self::Gt,
        Self::Le,
        Self::Ge,
        Self::NumEq,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::Le => "<=",
            Self::Ge => ">=",
            Self::NumEq => "=",
            Self::Equal => "equal?",
            Self::NotEqual => "not=",
        }
    }

    #[must_use]
    pub fn of(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    #[must_use]
    pub fn ints(self, a: i64, b: i64) -> Option<crate::value::Value> {
        use crate::value::Value::{Bool, Int};
        match self {
            Self::Add => a.checked_add(b).map(Int),
            Self::Sub => a.checked_sub(b).map(Int),
            Self::Mul => a.checked_mul(b).map(Int),
            Self::Lt => Some(Bool(a < b)),
            Self::Gt => Some(Bool(a > b)),
            Self::Le => Some(Bool(a <= b)),
            Self::Ge => Some(Bool(a >= b)),
            Self::NumEq | Self::Equal => Some(Bool(a == b)),
            Self::NotEqual => Some(Bool(a != b)),
        }
    }

    #[must_use]
    #[allow(clippy::float_cmp)]
    pub fn floats(self, a: f64, b: f64) -> crate::value::Value {
        use crate::value::Value::{Bool, Float};
        match self {
            Self::Add => Float(a + b),
            Self::Sub => Float(a - b),
            Self::Mul => Float(a * b),
            Self::Lt => Bool(a < b),
            Self::Gt => Bool(a > b),
            Self::Le => Bool(a <= b),
            Self::Ge => Bool(a >= b),
            Self::NumEq | Self::Equal => Bool(a == b),
            Self::NotEqual => Bool(a != b),
        }
    }
}
