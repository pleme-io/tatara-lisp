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
    // ── Exceptions / try-catch ────────────────────────────────────
    /// Push an error handler onto the current frame's handler stack.
    /// `catch_ip` is the absolute IP to jump to on error. `error_local`
    /// is the local slot the runtime stores the error Value into
    /// before resuming at `catch_ip`. Stack effect: 0.
    PushHandler {
        catch_ip: usize,
        error_local: usize,
    },
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
            Self::Return => -1,
            Self::MakeClosure(_) => 1,
            Self::MakeList(n) => 1 - *n as i32,
            Self::PushHandler { .. } | Self::PopHandler => 0,
            Self::Halt => 0,
        })
    }
}
