//! Bytecode VM — stack-based, side-by-side with the tree-walker.
//!
//! Architecture (per docs/vm-design.md):
//!
//! ```text
//!   Source text
//!     → read_spanned     (tatara-lisp reader)
//!   Spanned AST
//!     → fully_expand     (existing macro expander on Interpreter)
//!   Expanded Spanned
//!     → compile          (this module)
//!   Chunk { ops, consts, ip_to_span }
//!     → execute          (vm::Vm)
//!   Value
//! ```
//!
//! The VM shares the `Value` type with the tree-walker; the same
//! `FnRegistry` of native functions is consulted on `Call`. This means
//! every primitive (+, list, hash-map, http-get, ...) works uniformly
//! across both paths.
//!
//! Status: **Phase 1+2** — opcode set, compiler for arithmetic /
//! literals / let / if / begin / define / lambda / call / TAIL_CALL,
//! and the interpret loop. Closures use captured-env snapshots
//! (simpler than Lua's open/closed upvalue dance, sufficient for
//! embedded Lisp). Channels / macros / module loads / try-catch
//! flow through the existing tree-walker — the VM dispatches back
//! when it doesn't recognize a form.

pub mod chunk;
pub mod compile;
pub mod op;
pub mod run;

pub use chunk::{Chunk, ConstPool};
pub use compile::{CompileError, Compiler, compile_program};
pub use op::Op;
pub use run::{Vm, VmError};
