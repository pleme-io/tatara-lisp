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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::value::{Closure, PromiseState, Value};

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

pub struct Frame {
    bindings: Mutex<HashMap<Arc<str>, Value>>,
    /// The scope that pushed this frame has not ended yet.
    ///
    /// Set at creation, cleared by [`Env::release_scope_frames`] when the
    /// scope ends. While it is set, the scope's own exit is still coming and
    /// will run the release check itself, so
    /// [`Env::drop_releasing_defining_frame`] skips the frame instead of
    /// walking it on every call of a block defined inside a running function.
    /// Skipping only ever declines a release, so the flag cannot make one
    /// wrong.
    scope_open: AtomicBool,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            bindings: Mutex::default(),
            scope_open: AtomicBool::new(true),
        }
    }
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
    /// frame is released only when [`release_if_closed`] can
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
            if release_if_closed(frame, frames_pointing_at(self, frame)) {
                released += 1;
            }
        }
        released
    }

    /// Release the frames a scope pushed onto this environment, now that the
    /// scope is ending: every frame above the first `keep`, innermost first.
    /// Returns how many were released.
    ///
    /// [`Env::release_own_frames`] breaks the define-a-lambda ring once, when
    /// an interpreter dies. That ring is not a top-level phenomenon: a
    /// function body that binds a local lambda — `(define add (lambda …))`,
    /// a `let*` or `letrec` whose right-hand side is a lambda — builds the
    /// same Frame → Closure → Env → Frame ring in its own scope frame, once
    /// per call, and the call's `Env` going out of scope only drops one of the
    /// frame's handles. Measured in blue at about 1 KB per call, without
    /// bound, in a loop that otherwise holds nothing.
    ///
    /// So the evaluator calls this wherever a scope's frames die — a closure
    /// call returning, a `let` family form or a `catch` handler ending, a
    /// macro body finishing — with every value that outlives the scope (its
    /// result, a tail call's callee, a raised error) still alive. The same
    /// exclusivity equation as [`Env::release_own_frames`] then guards each
    /// frame, so a frame something outside still reaches is left as it is.
    ///
    /// Innermost first because releasing an inner frame drops the closures it
    /// bound, and those may be the very handles that held an outer one.
    /// Frames at or below the write floor, and the root frame, are never a
    /// scope's: they are the interpreter's globals and are released only when
    /// the interpreter dies.
    pub fn release_scope_frames(&self, keep: usize) -> usize {
        let floor = keep.max(self.write_floor + 1);
        let mut released = 0;
        for frame in self.frames.iter().skip(floor).rev() {
            frame.scope_open.store(false, Ordering::Release);
            if release_if_closed(frame, frames_pointing_at(self, frame)) {
                released += 1;
            }
        }
        released
    }

    /// Release the innermost frame, for a scope that pushed exactly one.
    /// See [`Env::release_scope_frames`].
    pub fn release_scope(&self) -> bool {
        self.release_scope_frames(self.frames.len().saturating_sub(1)) == 1
    }

    /// End a scope opened with [`Env::push`]: release its frame if nothing
    /// outside can reach it, then pop it. Call it with every value that
    /// outlives the scope still alive.
    pub fn pop_scope(&mut self) {
        self.release_scope();
        self.pop();
    }

    /// Drop `closure`, then release the frame it was defined in if this drop
    /// left that frame reachable only through its own bindings.
    ///
    /// The case [`Env::release_scope_frames`] cannot see: the scope ended
    /// while one of its own closures was still in flight, so its frame was
    /// correctly kept. A `letrec` loop in tail position does exactly this —
    /// it hands its own closure to the trampoline as the next call — and the
    /// frame stays held by that closure until the trampoline lets go of it,
    /// which is here.
    ///
    /// Only a closure held at most twice is looked at: by the caller and,
    /// possibly, by one binding inside the frame. One held more widely is
    /// still reachable after this drop whatever its frame holds, so the walk
    /// would be refused anyway.
    pub fn drop_releasing_defining_frame(closure: Arc<Closure>) {
        if Arc::strong_count(&closure) > 2 {
            return;
        }
        let env = &closure.captured_env;
        if env.frames.len() <= env.write_floor + 1 {
            return;
        }
        let Some(frame) = env.frames.last().map(Arc::clone) else {
            return;
        };
        if frame.scope_open.load(Ordering::Acquire) {
            return;
        }
        drop(closure);
        release_if_closed(&frame, 1);
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
/// [`release_if_closed`] balances handles, not environments.
fn frames_pointing_at(env: &Env, frame: &Arc<Frame>) -> usize {
    env.frames.iter().filter(|f| Arc::ptr_eq(f, frame)).count()
}

/// Clear `frame`'s bindings if nothing but the caller's `held` handles and
/// the frame's own contents can reach it. Returns whether it did.
///
/// One equation decides it:
///
/// ```text
/// Arc::strong_count(frame) == held + handles held inside frame
/// ```
///
/// It is sound because the right-hand side can only ever be an
/// *under*-count, and an under-count fails the equation, which refuses the
/// release. Every handle counted sits behind an `Arc` whose strong count is
/// exactly 1, so there is precisely one path to it and it cannot be counted
/// twice. Anything [`handles_to_frame`] cannot see through — a
/// `Value::Foreign` the host defined, a value something else also holds, a
/// persistent list — contributes zero to the right-hand side while still
/// contributing to `strong_count`, so it makes the equation fail rather than
/// pass.
///
/// When the count is already `held`, nothing inside the frame can hold a
/// handle either (it would be counted), so the equation holds with no walk.
/// That is the common case — a call that bound no closure — and it costs one
/// lock and the map drop the frame's own drop would have paid.
fn release_if_closed(frame: &Arc<Frame>, held: usize) -> bool {
    let strong = Arc::strong_count(frame);
    // A poisoned frame is one whose contents cannot be trusted to be walked;
    // refuse rather than guess.
    let Ok(bindings) = frame.bindings.lock() else {
        return false;
    };
    if strong != held {
        let mut aliased: HashMap<*const Closure, (usize, &Arc<Closure>)> = HashMap::new();
        let mut internal = 0usize;
        for value in bindings.values() {
            match value {
                Value::Closure(c) if Arc::strong_count(c) > 1 => {
                    aliased.entry(Arc::as_ptr(c)).or_insert((0, c)).0 += 1;
                }
                other => internal += handles_to_frame(frame, other, MAX_VALUE_DEPTH),
            }
        }
        internal += aliased
            .values()
            .filter(|(slots, c)| Arc::strong_count(c) == *slots)
            .map(|(_, c)| frames_pointing_at(&c.captured_env, frame))
            .sum::<usize>();
        if strong != held + internal {
            return false;
        }
    }
    // Take the map out from under the lock, then drop it with the guard
    // already gone. The values being dropped own `Env`s that own
    // `Arc<Frame>`s, and releasing the last handle to a frame drops ITS map in
    // turn — an unbounded drop chain that has no business running inside this
    // critical section.
    let mut bindings = bindings;
    let drained = std::mem::take(&mut *bindings);
    drop(bindings);
    drop(drained);
    true
}

/// How many `Arc<Frame>` handles to `frame` live inside `value`, counting
/// only what can be proven to sit on exactly one path from the frame.
///
/// Read the arms with one question in mind — *can this ever return MORE
/// handles than really exist?* It cannot, because there are only two shapes
/// of arm and neither can over-count:
///
/// - **descend** — permitted only through an `Arc` whose strong count is 1, so
///   the value has exactly one owner, lies on exactly one path, and cannot be
///   reached and counted a second time;
/// - **zero** — a variant that carries no `Env`, a value something else also
///   holds, a payload this crate cannot see inside, a poisoned lock, or the
///   depth bound. If such a value *does* hold a handle, zero under-counts,
///   the equation fails, and the release is declined.
///
/// An aliased value counts zero rather than refusing the whole frame: a
/// parameter bound to a global function is aliased by the global and holds no
/// handle to this frame, and refusing on it would leave every frame that has
/// one in a ring.
///
/// Lists and maps are never descended. They are persistent: two lists built
/// one from the other share their nodes, so an element is reachable from both
/// while the element's own `Arc` count stays 1. Counting through them counted
/// one closure twice and released a frame a live closure was still reading
/// (`tests/scope_release.rs`).
fn handles_to_frame(frame: &Arc<Frame>, value: &Value, depth: usize) -> usize {
    let Some(next) = depth.checked_sub(1) else {
        return 0;
    };
    match value {
        // The cycle `sf_define` builds, and the one this whole operation is
        // for: the frame owns the closure, the closure owns an env, that env
        // owns the frame.
        Value::Closure(c) if Arc::strong_count(c) == 1 => {
            frames_pointing_at(&c.captured_env, frame)
        }
        // The same ring through `sf_delay`: a pending promise's thunk is a
        // closure over the env the promise is bound in.
        Value::Promise(p) if Arc::strong_count(p) == 1 => {
            let Ok(state) = p.lock() else {
                return 0;
            };
            match &*state {
                PromiseState::Pending(thunk) if Arc::strong_count(thunk) == 1 => {
                    frames_pointing_at(&thunk.captured_env, frame)
                }
                PromiseState::Pending(_) => 0,
                PromiseState::Forced(v) => handles_to_frame(frame, v, next),
            }
        }
        Value::Error(e) if Arc::strong_count(e) == 1 => e
            .data
            .iter()
            .map(|(k, v)| handles_to_frame(frame, k, next) + handles_to_frame(frame, v, next))
            .sum(),
        // The VM's callable is a `Value::Foreign`, not a `Value::Closure` —
        // `CompiledClosure` carries a snapshot of the globals env, so a
        // VM-defined function closes the identical ring through a variant that
        // reads as opaque. Downcasting is not a special case for the VM so
        // much as the reason `Foreign` cannot simply be assumed inert.
        Value::Foreign(any) => match any.downcast_ref::<crate::vm::run::CompiledClosure>() {
            Some(cc) if Arc::strong_count(any) == 1 => {
                let mut n = frames_pointing_at(&cc.globals, frame);
                for cell in &cc.captures {
                    if Arc::strong_count(cell) == 1 {
                        if let Ok(captured) = cell.lock() {
                            n += handles_to_frame(frame, &captured, next);
                        }
                    }
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
        // Aliased, so on more than one path; persistent, so possibly on more
        // than one path whatever its own count says; or carrying no `Env` by
        // construction (a scalar, an interned name, a registry key, source
        // text). All three count zero.
        Value::Closure(_)
        | Value::Promise(_)
        | Value::List(_)
        | Value::Map(_)
        | Value::Error(_)
        | Value::Nil
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Str(_)
        | Value::Symbol(_)
        | Value::Keyword(_)
        | Value::NativeFn(_)
        | Value::Sexp(..) => 0,
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
