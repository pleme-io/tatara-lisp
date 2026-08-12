//! A dead interpreter must actually free its own frames.
//!
//! Every top-level `(define (f …) …)` builds a reference CYCLE: the frame owns
//! the closure (`Frame.bindings`), the closure owns the environment
//! (`Closure::captured_env`), and the environment owns the frame
//! (`Env.frames: Vec<Arc<Frame>>`). `Arc` is a refcount, not a collector, so
//! that ring is unreachable and unreclaimed for the life of the process. The
//! crate carries zero `Weak` and, before this file, zero `Drop`.
//!
//! The observable is a `Value::Foreign` sentinel bound in the same frame: it is
//! freed exactly when the frame is, so a flag flipped from its `Drop` reports
//! the frame's fate without reaching into the allocator.
//!
//! ## Red run, recorded (2026-08-12, rustc 1.97.1)
//!
//! Against the tree before `Env::release_own_frames` existed. Five of the seven
//! gates below were runnable then — the two that name the new API could not
//! compile, and were added after:
//!
//! ```text
//! test a_dead_forks_own_frame_is_reclaimed ... FAILED
//! test a_live_descendant_fork_still_reads_the_frame ... ok
//! test a_promise_thunk_does_not_pin_the_frame_either ... FAILED
//! test an_escaped_closure_keeps_the_frame_it_needs ... ok
//! test the_control_reclaims_without_any_closure ... ok
//! test result: FAILED. 3 passed; 2 failed
//!
//! ---- a_dead_forks_own_frame_is_reclaimed ----
//! the frame the closure captured was never freed — Frame → Closure → Env →
//! Frame is a refcount cycle and nothing breaks it
//! ```
//!
//! The CONTROL passing on that same run is what makes the failure mean
//! something: it proves the sentinel can be observed being dropped at all, so
//! the red arm is reporting a retained frame rather than a probe that never
//! observes anything. A gate whose control is not also run is a gate that
//! would pass just as happily against a sentinel nothing ever drops.
//!
//! ## Green run, after (2026-08-12)
//!
//! ```text
//! test result: ok. 7 passed; 0 failed
//! ```
//!
//! ## Second red run: the GUARD is load-bearing, not decoration
//!
//! `Env::release_own_frames` is guarded by an exclusivity proof, and the
//! premise it was nearly shipped on — "a frame above the write floor is
//! private to the fork that pushed it" — is true only at the moment the fork
//! is made. With `frame_is_exclusively_ours` short-circuited to `true`, same
//! tree, the two soundness arms below go red and the two leak arms go green:
//!
//! ```text
//! test a_dead_forks_own_frame_is_reclaimed ... ok
//! test a_live_descendant_fork_still_reads_the_frame ... FAILED
//! test a_promise_thunk_does_not_pin_the_frame_either ... ok
//! test an_escaped_closure_keeps_the_frame_it_needs ... FAILED
//! …the inherited binding must survive: UnboundSymbol { name: "f", … }
//! …a closure that outlived its interpreter is still callable:
//!   UnboundSymbol { name: "countdown", … }
//! test result: FAILED. 4 passed; 2 failed
//! ```
//!
//! That is the whole reason the release is conservative. Neither failure is a
//! memory-safety fault — `Arc` keeps the allocation alive either way — which
//! is exactly what makes it dangerous: an unguarded release does not crash, it
//! silently unbinds a name something live is still resolving.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tatara_lisp_eval::{install_full_stdlib_with, EvalError, Interpreter, Value};

/// Freed-or-not, reported by `Drop`. Held as a `Value::Foreign` so it lives in
/// the interpreter's frame exactly like a user binding does.
struct Sentinel(Arc<AtomicBool>);

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn prototype() -> Interpreter<()> {
    let mut interp = Interpreter::new();
    let mut host = ();
    install_full_stdlib_with(&mut interp, &mut host);
    interp
}

fn eval(interp: &mut Interpreter<()>, src: &str) -> Result<Value, EvalError> {
    let forms = tatara_lisp_eval::read_spanned(src).expect("parse");
    let mut host = ();
    interp.eval_program(&forms, &mut host)
}

/// Fork a child, plant the sentinel in its private frame, run `src`, kill it.
/// Returns the flag the sentinel's `Drop` would have flipped.
fn run_then_kill(src: &str) -> Arc<AtomicBool> {
    let freed = Arc::new(AtomicBool::new(false));
    let proto = prototype();

    let mut child = proto.fork();
    child.define_global("sentinel", Value::Foreign(Arc::new(Sentinel(freed.clone()))));
    eval(&mut child, src).expect("the program under test must run");
    drop(child);

    freed
}

// ── the control ────────────────────────────────────────────────────────────

/// **CONTROL.** The identical shape with no closure in it, so no cycle. This
/// reclaims before the fix and after it, and its only job is to prove the
/// sentinel is a working probe.
///
/// Without this arm the gate below would read exactly the same against a
/// sentinel that is never dropped for some unrelated reason — it would be
/// measuring nothing and reporting a pass. That trap has already been hit
/// downstream once.
#[test]
fn the_control_reclaims_without_any_closure() {
    let freed = run_then_kill("(define n 1)");
    assert!(
        freed.load(Ordering::SeqCst),
        "a frame holding no closure must be freed when its interpreter dies — \
         if this fails the probe is broken, not the fix"
    );
}

// ── the defect ─────────────────────────────────────────────────────────────

/// The gate. One top-level lambda is enough: `sf_define` builds the closure
/// with `captured_env: env.clone()` and then defines it back INTO that same
/// env, so the frame holds itself alive.
#[test]
fn a_dead_forks_own_frame_is_reclaimed() {
    let freed = run_then_kill("(define (f n) n)");
    assert!(
        freed.load(Ordering::SeqCst),
        "the frame the closure captured was never freed — Frame → Closure → \
         Env → Frame is a refcount cycle and nothing breaks it"
    );
}

/// The same ring through `(delay …)`: the promise's thunk is a closure over
/// the defining env, and the promise is bound back into it.
#[test]
fn a_promise_thunk_does_not_pin_the_frame_either() {
    let freed = run_then_kill("(define p (delay (+ 1 2)))");
    assert!(
        freed.load(Ordering::SeqCst),
        "a pending promise's thunk captures the env the promise is bound in"
    );
}

// ── what the release must NOT do ───────────────────────────────────────────

/// **SOUNDNESS.** A frame above the write floor is private to the fork that
/// pushed it — but a fork OF that fork inherits it, below its own floor. The
/// releasing interpreter is then not the only reader, and clearing the frame
/// would silently unbind names a live descendant is still resolving.
///
/// Red run for this arm: with the exclusivity guard removed from
/// `Env::release_own_frames`, this fails with `UnboundSymbol { name: "f" }`.
#[test]
fn a_live_descendant_fork_still_reads_the_frame() {
    let proto = prototype();

    let mut parent = proto.fork();
    eval(&mut parent, "(define (f n) (* n 2))").expect("parent process defines");

    let mut child = parent.fork();
    drop(parent); // the parent incarnation dies; the child is still running

    let got = eval(&mut child, "(f 21)").expect("the inherited binding must survive");
    assert!(
        matches!(got, Value::Int(42)),
        "a descendant fork reads the dead parent's frame below its own floor, \
         so releasing it would unbind a live name; got {got:?}"
    );
}

/// **SOUNDNESS.** A closure handed out to the embedder outlives the
/// interpreter that made it, and it resolves its own name through the very
/// frame the release would clear. The guard must see the extra handle and
/// leave the frame alone.
///
/// Red run for this arm: with the guard removed, the recursive call fails with
/// `UnboundSymbol { name: "countdown" }`.
#[test]
fn an_escaped_closure_keeps_the_frame_it_needs() {
    let mut host = ();
    let mut outer = prototype();

    let escaped = {
        let mut child = outer.fork();
        eval(
            &mut child,
            "(define (countdown n) (if (= n 0) 0 (countdown (- n 1)))) countdown",
        )
        .expect("define and return it")
    };
    // `child` is dead here; `escaped` is not.

    let got = outer
        .apply_external_value(
            &escaped,
            vec![Value::Int(5)],
            &mut host,
            tatara_lisp_eval::Span::synthetic(),
        )
        .expect("a closure that outlived its interpreter is still callable");
    assert!(
        matches!(got, Value::Int(0)),
        "the escaped closure resolves its OWN name through the frame it was \
         defined in; got {got:?}"
    );
}

/// A host handle bound in the frame AND still held by the embedder must not
/// block reclamation. It carries no `Env`, so it cannot be part of any ring —
/// and refusing on it would make the release useless to exactly the embedders
/// that hand typed Rust handles to Lisp code, which is most of them.
///
/// The discriminator is what happens on the SECOND drop: if the frame were
/// retained it would still be holding a handle of its own, and dropping the
/// embedder's would not be dropping the last one.
///
/// Red run for this arm: with `handles_to_frame`'s `Foreign` arm refusing
/// (`return None`) instead of under-counting (`0`) for a payload it cannot see
/// through, the final assertion fails — `dropping the embedder's handle must
/// drop the LAST one`. (That mutation also reddens the two leak arms above,
/// since their sentinel is a `Foreign` too, which is the point: refusing on
/// `Foreign` refuses on almost every real frame.)
#[test]
fn a_host_handle_the_embedder_also_holds_does_not_block_release() {
    let freed = Arc::new(AtomicBool::new(false));
    let proto = prototype();

    let handle: Arc<dyn std::any::Any + Send + Sync> = Arc::new(Sentinel(freed.clone()));
    let mut child = proto.fork();
    child.define_global("handle", Value::Foreign(handle.clone()));
    eval(&mut child, "(define (f n) n)").expect("the program under test must run");
    drop(child);

    assert!(
        !freed.load(Ordering::SeqCst),
        "the embedder still holds it, so it must NOT have been dropped yet"
    );

    drop(handle);
    assert!(
        freed.load(Ordering::SeqCst),
        "dropping the embedder's handle must drop the LAST one — a retained \
         frame would still be holding one of its own"
    );
}

// ── the explicit surface ───────────────────────────────────────────────────

/// The release is a named operation, not only a destructor, so an embedder
/// that knows a child is finished can reclaim without waiting for the drop —
/// and so the guard's verdict is observable in a test rather than inferred.
#[test]
fn release_own_frames_reports_what_it_released() {
    use tatara_lisp_eval::{Env, Seal};

    let parent = Env::new();
    parent.define("inherited", Value::Int(1));

    let mut child = parent.sealed_below_top(Seal::Fork);
    child.define("mine", Value::Int(2));

    assert_eq!(child.release_own_frames(), 1, "the one private frame");
    assert!(child.lookup("mine").is_none(), "its bindings are gone");
    assert!(
        matches!(child.lookup("inherited"), Some(Value::Int(1))),
        "and the shared frames below the floor are untouched"
    );
}
