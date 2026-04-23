//! Lexical environment holding `Value`s.
//!
//! Mirrors `tatara_lisp::Env` in shape but stores runtime values rather
//! than source forms. Frames are pushed on lambda invocation and popped
//! on return; closures capture a clone of the `Env` at lambda-creation.

use std::collections::HashMap;
use std::sync::Arc;

use crate::value::Value;

/// A lexically-scoped environment of `Arc<str>` names to `Value`s.
#[derive(Clone, Debug, Default)]
pub struct Env {
    frames: Vec<HashMap<Arc<str>, Value>>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            frames: vec![HashMap::new()],
        }
    }

    pub fn push(&mut self) {
        self.frames.push(HashMap::new());
    }

    pub fn pop(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    /// Bind `name` in the innermost frame.
    pub fn define(&mut self, name: impl Into<Arc<str>>, value: Value) {
        if let Some(top) = self.frames.last_mut() {
            top.insert(name.into(), value);
        }
    }

    /// Look up `name`, walking from innermost to outermost frame.
    pub fn lookup(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.get(name) {
                return Some(v);
            }
        }
        None
    }

    /// Mutate an existing binding in the nearest enclosing frame. Returns
    /// `false` if no such binding exists (caller should surface as error).
    pub fn set(&mut self, name: &str, value: Value) -> bool {
        for frame in self.frames.iter_mut().rev() {
            if let Some(slot) = frame.get_mut(name) {
                *slot = value;
                return true;
            }
        }
        false
    }

    pub fn frame_depth(&self) -> usize {
        self.frames.len()
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
        let mut env = Env::new();
        env.define("x", Value::Int(1));
        assert!(env.set("x", Value::Int(99)));
        assert!(matches!(env.lookup("x"), Some(Value::Int(99))));
        assert!(!env.set("no-such", Value::Nil));
    }
}
