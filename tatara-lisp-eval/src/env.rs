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

use crate::value::{PromiseState, Value};

/// How deep [`handles_to_frame`] will walk a value graph before giving up.
///
/// Giving up is the conservative answer — it refuses a release rather than
/// permitting one — so the only cost of the bound is a frame that keeps
/// leaking exactly as it does today. Far above any hand-built nesting; far
/// below the recursion depth that would blow the stack inside a destructor,
/// which is the one place a crash has no useful backtrace.
const MAX_VALUE_DEPTH: usize = 64;

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

    /// Release the frames this environment OWNS: drop the bindings of every
    /// frame at or above its write floor, so the frames themselves can be
    /// freed. Returns how many were released.
    ///
    /// # The cycle this exists to break
    ///
    /// A top-level `(define (f …) …)` builds a ring. `sf_define` clones the
    /// env INTO the closure (`Closure::captured_env`) and then defines the
    /// closure back into that same env, so the `Frame` owns the
    /// `Value::Closure`, the closure owns the `Env`, and the `Env` owns the
    /// `Arc<Frame>`. `Arc` is a refcount, not a collector: that ring's strong
    /// count never reaches zero, and nothing in this crate breaks it — there
    /// is no `Weak` anywhere in it. Measured downstream at ~832 B per dead
    /// interpreter incarnation, which a supervisor restarting one process a
    /// second turns into tens of megabytes a day.
    ///
    /// # Why the write floor is the boundary
    ///
    /// [`Env::sealed_below_top`] clones the frame vector, raises the floor to
    /// its length, and pushes ONE fresh frame. So from `write_floor` up sits a
    /// frame this environment created and handed to nobody, and below it sit
    /// the frames the parent and every sibling share. Releasing only from the
    /// floor up is what keeps the stdlib a hundred forks are reading out of
    /// scope entirely.
    ///
    /// # Why that is not sufficient on its own
    ///
    /// "Created here" is not "held only here", and there are two real ways a
    /// frame above the floor becomes somebody else's business:
    ///
    /// 1. **a fork of a fork** inherits it *below its own floor* and is still
    ///    resolving names through it;
    /// 2. **a closure returned to the embedder** outlives this environment and
    ///    resolves its own name through it.
    ///
    /// Neither is a memory-safety problem — `Arc` keeps the allocation alive
    /// whatever we do — but clearing the map would silently unbind names
    /// something live is still reading, which is worse than the leak. So a
    /// frame is released only when [`Env::frame_is_exclusively_ours`] can
    /// *prove* nothing outside this environment and its own self-referential
    /// bindings can reach it; when the proof fails the frame is left exactly
    /// as it is today. That trades completeness for soundness, never the other
    /// way round.
    ///
    /// Both hazards are gated in `tests/interpreter_release.rs`, each with the
    /// recorded red run it goes red on when the guard is removed.
    pub fn release_own_frames(&mut self) -> usize {
        let mut released = 0;
        for frame in &self.frames[self.write_floor..] {
            if Self::frame_is_exclusively_ours(frame) {
                // Take the map out from under the lock, then drop it with the
                // guard already gone. The values being dropped own `Env`s that
                // own `Arc<Frame>`s, and releasing the last handle to a frame
                // drops ITS map in turn — an unbounded drop chain that has no
                // business running inside this critical section.
                let drained = {
                    let mut bindings = frame.bindings.lock().unwrap();
                    std::mem::take(&mut *bindings)
                };
                drop(drained);
                released += 1;
            }
        }
        released
    }

    /// Is `frame` reachable from anywhere but this `Env` and the bindings
    /// `frame` itself holds?
    ///
    /// One equation decides it:
    ///
    /// ```text
    /// Arc::strong_count(frame) == 1 (this Env) + handles held inside frame
    /// ```
    ///
    /// It is sound because the right-hand side can only ever be an
    /// *under*-count, and an under-count fails the equation, which refuses the
    /// release. Every handle counted sits behind an `Arc` whose strong count
    /// is exactly 1, so there is precisely one path to it and it cannot be
    /// counted twice. Anything [`handles_to_frame`] cannot see through — a
    /// `Value::Foreign` the host defined, a capture cell something else also
    /// holds — contributes zero to the right-hand side while still
    /// contributing to `strong_count`, so it makes the equation fail rather
    /// than pass.
    fn frame_is_exclusively_ours(frame: &Arc<Frame>) -> bool {
        // A poisoned frame is one whose contents cannot be trusted to be
        // walked; refuse rather than guess.
        let Ok(bindings) = frame.bindings.lock() else {
            return false;
        };
        let mut internal = 0usize;
        for value in bindings.values() {
            let Some(n) = handles_to_frame(frame, value, MAX_VALUE_DEPTH) else {
                return false;
            };
            internal += n;
        }
        Arc::strong_count(frame) == 1 + internal
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

/// How many of `env`'s frame slots are this very `frame`.
///
/// A count rather than a bool because the same `Env` may legitimately hold
/// several distinct handles to one frame, and the exclusivity equation in
/// [`Env::frame_is_exclusively_ours`] balances handles, not environments.
fn frames_pointing_at(env: &Env, frame: &Arc<Frame>) -> usize {
    env.frames.iter().filter(|f| Arc::ptr_eq(f, frame)).count()
}

/// How many `Arc<Frame>` handles to `frame` live inside `value` — or `None`
/// when `value`'s graph holds something this function cannot account for.
///
/// `None` means "do not release": the caller treats it as a refusal. That is
/// the whole safety argument, so read the arms with one question in mind —
/// *can this ever return MORE handles than really exist?* It cannot, because
/// there are only three shapes of arm and none of them can over-count:
///
/// - **descend** — permitted only through an `Arc` whose strong count is 1, so
///   the value has exactly one owner, lies on exactly one path, and cannot be
///   reached and counted a second time;
/// - **refuse** (`None`) — an aliased value, a poisoned lock, or the depth
///   bound: no count is produced at all;
/// - **zero** — a variant that carries no `Env`, or a `Foreign` payload this
///   crate cannot see inside. If such a payload *does* hold a handle, zero
///   under-counts, the equation fails, and the release is declined.
fn handles_to_frame(frame: &Arc<Frame>, value: &Value, depth: usize) -> Option<usize> {
    // The depth bound, spelled as a refusal: at zero this returns `None`, which
    // the caller reads as "cannot prove exclusivity" and declines to release.
    let next = depth.checked_sub(1)?;
    Some(match value {
        // The cycle `sf_define` builds, and the one this whole operation is
        // for: the frame owns the closure, the closure owns an env, that env
        // owns the frame.
        Value::Closure(c) if Arc::strong_count(c) == 1 => {
            frames_pointing_at(&c.captured_env, frame)
        }
        // The same ring through `sf_delay`: a pending promise's thunk is a
        // closure over the env the promise is bound in.
        Value::Promise(p) if Arc::strong_count(p) == 1 => {
            let state = p.lock().ok()?;
            match &*state {
                PromiseState::Pending(thunk) if Arc::strong_count(thunk) == 1 => {
                    frames_pointing_at(&thunk.captured_env, frame)
                }
                PromiseState::Pending(_) => return None,
                PromiseState::Forced(v) => handles_to_frame(frame, v, next)?,
            }
        }
        Value::List(xs) if Arc::strong_count(xs) == 1 => {
            let mut n = 0;
            for x in xs.as_ref() {
                n += handles_to_frame(frame, x, next)?;
            }
            n
        }
        Value::Map(m) if Arc::strong_count(m) == 1 => {
            let mut n = 0;
            for v in m.values() {
                n += handles_to_frame(frame, v, next)?;
            }
            n
        }
        Value::Error(e) if Arc::strong_count(e) == 1 => {
            let mut n = 0;
            for (k, v) in &e.data {
                n += handles_to_frame(frame, k, next)?;
                n += handles_to_frame(frame, v, next)?;
            }
            n
        }
        // The VM's callable is a `Value::Foreign`, not a `Value::Closure` —
        // `CompiledClosure` carries a snapshot of the globals env, so a
        // VM-defined function closes the identical ring through a variant that
        // reads as opaque. Downcasting is not a special case for the VM so
        // much as the reason `Foreign` cannot simply be assumed inert.
        Value::Foreign(any) => match any.downcast_ref::<crate::vm::run::CompiledClosure>() {
            Some(cc) if Arc::strong_count(any) == 1 => {
                let mut n = frames_pointing_at(&cc.globals, frame);
                for cell in &cc.captures {
                    if Arc::strong_count(cell) != 1 {
                        return None;
                    }
                    let captured = cell.lock().ok()?;
                    n += handles_to_frame(frame, &captured, next)?;
                }
                n
            }
            // An aliased compiled closure, or any other host-owned payload.
            // Zero here is deliberate rather than lazy: it is the under-count,
            // and an under-count refuses the release. So a host that boxes an
            // `Env` in a `Foreign` costs us the leak, never a cleared frame it
            // is still reading — while a host handle that boxes no `Env` at
            // all, which is nearly all of them, stops blocking reclamation of
            // the frame it happens to be bound in.
            _ => 0,
        },
        // Aliased, so on more than one path and not provably ours.
        Value::Closure(_)
        | Value::Promise(_)
        | Value::List(_)
        | Value::Map(_)
        | Value::Error(_) => return None,
        // Carries no `Env`, by construction: every remaining variant is a
        // scalar, an interned name, a registry key, or source text.
        Value::Nil
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Str(_)
        | Value::Symbol(_)
        | Value::Keyword(_)
        | Value::NativeFn(_)
        | Value::Sexp(..) => 0,
    })
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
