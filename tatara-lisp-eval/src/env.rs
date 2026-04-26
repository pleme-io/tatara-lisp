//! Lexical environment holding `Value`s.
//!
//! Each frame is an `Arc<RefCell<HashMap>>`. Cloning an `Env` Arc-clones
//! the frames, so a closure that captures the env at creation shares
//! state with subsequent definitions in those same frames — which is
//! what makes top-level recursion and mutual recursion work: the closure
//! looks up its own name in a frame that the enclosing `define` later
//! populates.
//!
//! Not `Sync` (`RefCell`). Single-threaded eval is the expected mode;
//! if cross-thread use ever needs to share a Value, migrate frames to
//! `Arc<Mutex<...>>`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::value::Value;

#[derive(Default)]
pub struct Frame {
    bindings: RefCell<HashMap<Arc<str>, Value>>,
}

impl Frame {
    fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("len", &self.bindings.borrow().len())
            .finish()
    }
}

/// A lexically-scoped environment of `Arc<str>` names to `Value`s. Frames
/// are shared via `Arc`, so mutations to a frame are visible to every
/// `Env` holding the same frame.
#[derive(Clone, Debug)]
pub struct Env {
    frames: Vec<Arc<Frame>>,
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
        }
    }

    /// Push a fresh innermost frame, for `let` / lambda body scope.
    pub fn push(&mut self) {
        self.frames.push(Arc::new(Frame::new()));
    }

    /// Drop the innermost frame. No-op if only the root frame remains.
    pub fn pop(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    /// Bind `name` in the innermost frame. Shadows any outer binding.
    /// Visible to every other `Env` holding the same innermost frame.
    pub fn define(&self, name: impl Into<Arc<str>>, value: Value) {
        if let Some(top) = self.frames.last() {
            top.bindings.borrow_mut().insert(name.into(), value);
        }
    }

    /// Look up `name`, walking from innermost to outermost frame.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.bindings.borrow().get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    /// Mutate an existing binding in the nearest enclosing frame. Returns
    /// `false` if no such binding exists.
    pub fn set(&self, name: &str, value: Value) -> bool {
        for frame in self.frames.iter().rev() {
            let mut bindings = frame.bindings.borrow_mut();
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

    /// Iterate every binding in the OUTERMOST (root) frame as
    /// `(name, value)` pairs. Useful for module loaders that need to
    /// snapshot the top-level definitions a module evaluated to.
    /// Bindings in inner frames (let / lambda body) are excluded.
    pub fn iter_top_level(&self) -> Vec<(Arc<str>, Value)> {
        if let Some(root) = self.frames.first() {
            root.bindings
                .borrow()
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
