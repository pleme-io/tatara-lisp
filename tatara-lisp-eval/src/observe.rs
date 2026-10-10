//! Watching closure calls: the one place a profiler, a coverage counter, a
//! tracer and an error's call stack all read from.
//!
//! Every closure call the tree-walker makes passes one site,
//! `eval::call_closure_body`, once per activation — a tail call included,
//! since the trampoline re-enters the same loop. That site has two arms:
//!
//! - **the observer arm**, an optional [`CallObserver`] the host installs with
//!   [`crate::Interpreter::set_call_observer`]. It sees each activation enter
//!   with its arguments and leave with how it left ([`Exit`]), and may answer
//!   a call itself instead of letting the body run. With none installed, the
//!   cost is one `Option` test per call;
//! - **the unwind arm**, always on, because it costs nothing until something
//!   fails: an error leaving an activation records that activation as a
//!   [`TraceFrame`], innermost first, and the host reads the result with
//!   [`crate::Interpreter::take_trace`]. A `try` that catches the error
//!   discards what it recorded, since the frames belong to an error nobody
//!   will report.
//!
//! The tree-walker only. The VM's frames are its own and report neither arm.

use std::sync::{Arc, Mutex, PoisonError};

use tatara_lisp::Span;

use crate::error::EvalError;
use crate::value::Value;

/// One closure activation as the evaluator sees it.
#[derive(Clone, Copy, Debug)]
pub struct Call<'a> {
    /// The name `(define (name …) …)` gave the closure; `None` for a lambda.
    pub name: Option<&'a Arc<str>>,
    /// Where the closure was defined: the `define` or `lambda` form.
    pub source: Span,
    /// Where this activation was called from.
    pub at: Span,
}

/// How an activation left.
#[derive(Clone, Copy, Debug)]
pub enum Exit<'a> {
    Returned(&'a Value),
    /// Its last form called another closure in tail position, which replaces
    /// it: the callee enters next, as a sibling rather than a child.
    TailCalled,
    Raised(&'a EvalError),
    /// The observer's `enter` answered for it, and its body never ran.
    Supplied(&'a Value),
}

/// A host's view of closure calls, and its one say in them. Each `enter` is
/// followed by exactly one `leave` for the same activation, on every path out
/// of it.
pub trait CallObserver: Send + Sync {
    /// Before the body runs, with the arguments bound. `None` lets it run;
    /// `Some(value)` makes `value` the call's result without running the body
    /// (a memo table answering), and `leave` then sees [`Exit::Supplied`].
    fn enter(&self, call: &Call<'_>, args: &[Value]) -> Option<Value>;
    fn leave(&self, call: &Call<'_>, exit: Exit<'_>);
}

/// One activation an error unwound through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceFrame {
    pub name: Option<Arc<str>>,
    pub source: Span,
    pub at: Span,
}

/// The activations an error unwound through, innermost first.
///
/// Bounded: past [`TRACE_FRAMES`] only the count grows, so a recursion refused
/// at a depth of 100,000 costs a counter, not 100,000 frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    pub frames: Vec<TraceFrame>,
    /// Every activation unwound, kept or not.
    pub depth: usize,
}

/// How many frames a [`Trace`] keeps.
pub const TRACE_FRAMES: usize = 256;

/// The unwind arm's record. Per incarnation, like the budget's counters: a
/// fork starts with none.
#[derive(Default)]
pub(crate) struct Unwind(Mutex<Trace>);

impl Unwind {
    pub(crate) fn push(&self, call: &Call<'_>) {
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        t.depth += 1;
        if t.frames.len() < TRACE_FRAMES {
            t.frames.push(TraceFrame {
                name: call.name.cloned(),
                source: call.source,
                at: call.at,
            });
        }
    }

    pub(crate) fn clear(&self) {
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if t.depth > 0 {
            *t = Trace::default();
        }
    }

    pub(crate) fn take(&self) -> Trace {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}
