//! Lexical environment holding `Value`s.
//!
//! Each frame is an `Arc<Mutex<HashMap>>`. Cloning an `Env` Arc-clones
//! the frames, so a closure that captures the env at creation shares
//! state with subsequent definitions in those same frames — which is
//! what makes top-level recursion and mutual recursion work: the closure
//! looks up its own name in a frame that the enclosing `define` later
//! populates.
//!
//! `Send + Sync` (`Mutex`). Single-threaded eval is the expected mode,
//! but Send + Sync is required to make `Value` itself Send + Sync,
//! which is in turn required so channels can carry closures across
//! cooperative tasks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::value::Value;

/// Why an environment's lower frames are closed to `set!`.
///
/// The reason is carried rather than assumed because two callers now seal, and
/// the refusal message has to be true for whichever one hit it. It was written
/// for the first — it opened "cannot `set!` … from a macro body" — and telling
/// a forked process that its own code is a macro body sends the reader to
/// inspect the wrong thing entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seal {
    /// [`Env::sealed_below_top`] for macro expansion: a macro body may read
    /// every global and may not write one, so expansion is deterministic and
    /// compile-time state cannot outlive it.
    MacroExpansion,
    /// [`Env::sealed_below_top`] for a forked child: the shared frames belong
    /// to the parent and to every sibling, so a write through them is not
    /// this child's to make.
    Fork,
}

impl Seal {
    /// The refusal, in the terms of whoever hit it. Returned as a `&'static
    /// str` so the message is chosen by the variant rather than assembled at
    /// the raise site — the shape that let the macro wording reach a forked
    /// process in the first place.
    #[must_use]
    pub const fn refusal(self) -> &'static str {
        match self {
            Self::MacroExpansion => {
                "it is bound outside the expansion and sealed. Macro expansion must be \
                 deterministic, so compile-time state cannot outlive the expansion. Use a \
                 local binding, or return the value in the expansion."
            }
            Self::Fork => {
                "it belongs to the parent environment this one was forked from, which every \
                 sibling also shares. Writing it would be visible outside this child. Shadow \
                 it with a local `define` instead."
            }
        }
    }
}

#[derive(Default)]
pub struct Frame {
    bindings: Mutex<HashMap<Arc<str>, Value>>,
}

impl Frame {
    fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("len", &self.bindings.lock().unwrap().len())
            .finish()
    }
}

/// A lexically-scoped environment of `Arc<str>` names to `Value`s. Frames
/// are shared via `Arc`, so mutations to a frame are visible to every
/// `Env` holding the same frame.
#[derive(Clone, Debug)]
pub struct Env {
    frames: Vec<Arc<Frame>>,
    /// Frames at index `< write_floor` are **read-only to `set!`**.
    ///
    /// Zero for an ordinary environment, so nothing changes on the normal
    /// path. Raised by [`Env::sealed_below_top`] to build a macro-time
    /// environment: a macro body may still *read* every global, but a
    /// `set!` that would walk out into the interpreter's own globals is
    /// refused instead of silently mutating compile-time state.
    ///
    /// Why this is needed at all: frames are shared via `Arc`, and
    /// `Env::set` takes `&self` and mutates *through* the `Arc`. So an
    /// immutable reference to a cloned `Env` was sufficient to write the
    /// compiler's globals — measured, and the reason macro expansion was
    /// not deterministic.
    ///
    /// `set!` is a `SpecialForm`, not a bindable name, so it cannot be
    /// withheld by leaving it out of an environment. The reachable frames
    /// are the only thing that can be restricted.
    write_floor: usize,
    /// Why the frames below `write_floor` are closed. `None` exactly when
    /// `write_floor == 0` — the two move together, which is why the
    /// constructor sets both and nothing else may.
    seal: Option<Seal>,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Self {
            frames: vec![Arc::new(Frame::new())],
            write_floor: 0,
            seal: None,
        }
    }

    /// Clone this environment with every CURRENT frame sealed against
    /// `set!` for `reason`, then push one fresh writable frame on top.
    ///
    /// The child can read every global and every stdlib function, can
    /// `define` freely in its own frame, and cannot reach outward to mutate
    /// state the parent or a sibling can observe. A `set!` targeting a sealed
    /// binding raises [`crate::EvalError::SetSealed`] carrying `reason`,
    /// rather than silently succeeding.
    ///
    /// `reason` is a parameter and not a constant because the refusal is read
    /// by a human who needs to know *which* boundary they hit — see [`Seal`].
    pub fn sealed_below_top(&self, reason: Seal) -> Self {
        let mut env = self.clone();
        env.write_floor = env.frames.len();
        env.seal = Some(reason);
        env.push();
        env
    }

    /// Why this environment's lower frames are closed, if they are.
    #[must_use]
    pub fn seal(&self) -> Option<Seal> {
        self.seal
    }

    /// Is `name` bound only in a frame sealed against `set!`?
    ///
    /// Distinguishes "refused because sealed" from "no such binding", so
    /// the caller can raise the right error.
    pub fn is_sealed_binding(&self, name: &str) -> bool {
        if self.write_floor == 0 {
            return false;
        }
        let writable_has_it = self.frames[self.write_floor..]
            .iter()
            .any(|f| f.bindings.lock().unwrap().contains_key(name));
        if writable_has_it {
            return false;
        }
        self.frames[..self.write_floor]
            .iter()
            .any(|f| f.bindings.lock().unwrap().contains_key(name))
    }

    /// Push a fresh innermost frame, for `let` / lambda body scope.
    pub fn push(&mut self) {
        self.frames.push(Arc::new(Frame::new()));
    }

    /// Drop the innermost frame. No-op if only the root frame remains, and
    /// no-op if popping would take the environment below its own write floor.
    ///
    /// The floor guard matters for a sealed environment: `define` writes to
    /// `frames.last()`, so popping away the one writable frame would silently
    /// redirect every subsequent `define` into a frame this environment is not
    /// allowed to write — mutating the parent's globals through the shared
    /// `Arc` and defeating the seal. Unbalanced push/pop is a caller bug, but
    /// this is the point where that bug would stop being detectable.
    ///
    /// For an unsealed environment (`write_floor == 0`) this is exactly the
    /// previous behaviour: `frames.len() > 1`.
    pub fn pop(&mut self) {
        if self.frames.len() > 1 && self.frames.len() > self.write_floor + 1 {
            self.frames.pop();
        }
    }

    /// Bind `name` in the innermost frame. Shadows any outer binding.
    /// Visible to every other `Env` holding the same innermost frame.
    pub fn define(&self, name: impl Into<Arc<str>>, value: Value) {
        if let Some(top) = self.frames.last() {
            top.bindings.lock().unwrap().insert(name.into(), value);
        }
    }

    /// Look up `name`, walking from innermost to outermost frame.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.bindings.lock().unwrap().get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    /// Mutate an existing binding in the nearest enclosing frame. Returns
    /// `false` if no such binding exists.
    /// Frames below `write_floor` are skipped — see the field's docs.
    pub fn set(&self, name: &str, value: Value) -> bool {
        for frame in self.frames[self.write_floor..].iter().rev() {
            let mut bindings = frame.bindings.lock().unwrap();
            if let Some(slot) = bindings.get_mut(name) {
                *slot = value;
                return true;
            }
        }
        false
    }

    pub fn frame_depth(&self) -> usize {
        self.frames.len()
    }

    /// Index of the first frame this environment may write via `set!`.
    /// Zero for an ordinary environment.
    pub fn write_floor(&self) -> usize {
        self.write_floor
    }

    /// Iterate every binding in the OUTERMOST (root) frame as
    /// `(name, value)` pairs. Useful for module loaders that need to
    /// snapshot the top-level definitions a module evaluated to.
    /// Bindings in inner frames (let / lambda body) are excluded.
    pub fn iter_top_level(&self) -> Vec<(Arc<str>, Value)> {
        if let Some(root) = self.frames.first() {
            root.bindings
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_walks_chain() {
        let mut env = Env::new();
        env.define("x", Value::Int(1));
        env.push();
        env.define("y", Value::Int(2));
        assert!(matches!(env.lookup("x"), Some(Value::Int(1))));
        assert!(matches!(env.lookup("y"), Some(Value::Int(2))));
        env.pop();
        assert!(env.lookup("y").is_none());
    }

    #[test]
    fn set_mutates_existing_binding() {
        let env = Env::new();
        env.define("x", Value::Int(1));
        assert!(env.set("x", Value::Int(99)));
        assert!(matches!(env.lookup("x"), Some(Value::Int(99))));
        assert!(!env.set("no-such", Value::Nil));
    }

    #[test]
    fn cloned_env_shares_frame_state() {
        // This is the invariant that makes top-level recursion work:
        // a closure captured via env.clone() sees subsequent defines on
        // the same innermost frame.
        let env_a = Env::new();
        let env_b = env_a.clone();
        env_a.define("x", Value::Int(42));
        assert!(matches!(env_b.lookup("x"), Some(Value::Int(42))));
    }

    #[test]
    fn push_after_clone_diverges() {
        // After push, env_a has its own new frame; env_b doesn't see it.
        let mut env_a = Env::new();
        let env_b = env_a.clone();
        env_a.push();
        env_a.define("only-in-a", Value::Int(7));
        assert!(matches!(env_a.lookup("only-in-a"), Some(Value::Int(7))));
        assert!(env_b.lookup("only-in-a").is_none());
    }
}
