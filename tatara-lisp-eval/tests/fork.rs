//! `Interpreter::fork` — the shared-globals, private-writes child.
//!
//! Every gate here has a red run recorded in the commit that added it. Two of
//! them exist specifically because the cheap version of this feature — a plain
//! `Clone` — passes the obvious tests and fails these: frames are `Arc`-shared
//! and `Env::set` writes *through* the `Arc`, so a clone shares globals in the
//! mutable direction and one child can edit another's stdlib.

use tatara_lisp_eval::{install_full_stdlib_with, EvalError, Interpreter, Seal, Value};

/// Build the prototype once: primitives, HOFs, maps, channels, fibers, and the
/// evaluated Lisp standard library. This is the ~945µs `fork` exists to avoid
/// paying per child.
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

fn int(v: Value) -> i64 {
    match v {
        Value::Int(n) => n,
        other => panic!("expected an integer, got {other:?}"),
    }
}

// ── what the child inherits ────────────────────────────────────────────────

/// The point of the whole exercise: a child can run a program against the full
/// standard library without having rebuilt it.
///
/// `->>`, `range` and `foldl` span all three installed layers — the Lisp
/// stdlib, the Rust HOFs, and the primitives — so a fork that dropped any one
/// of them fails here rather than at some consumer's call site. That is the
/// `install_primitives`-without-`install_hof` defect, gated.
#[test]
fn a_fork_runs_the_full_stdlib_it_never_built() {
    let mut child = prototype().fork();
    let sum = eval(&mut child, "(foldl + 0 (range 1 5))").expect("stdlib is reachable");
    assert_eq!(int(sum), 10, "1+2+3+4");
}

/// A fork shares frames rather than copying them. The evidence is temporal: a
/// `define` the parent runs *after* the fork is visible to the child. A copy
/// could not show that, so this distinguishes the two implementations without
/// timing anything.
#[test]
fn the_shared_frames_are_shared_not_copied() {
    let mut parent = prototype();
    let mut child = parent.fork();

    eval(&mut parent, "(define minted-after-the-fork 7)").expect("parent define");

    let seen = eval(&mut child, "minted-after-the-fork").expect("child sees it");
    assert_eq!(
        int(seen),
        7,
        "a copied environment could not observe a later parent define"
    );
}

// ── what the child cannot leak ─────────────────────────────────────────────

/// A child's own definitions stay in its own frame. This is the isolation the
/// test runner and the process supervisor both depend on.
#[test]
fn a_childs_define_is_invisible_to_the_parent_and_to_its_siblings() {
    let parent = prototype();
    let mut a = parent.fork();
    let mut b = parent.fork();

    eval(&mut a, "(define only-in-a 1)").expect("a defines");

    let in_b = eval(&mut b, "only-in-a");
    assert!(
        matches!(in_b, Err(EvalError::UnboundSymbol { .. })),
        "a sibling must not see it, got {in_b:?}"
    );

    // The parent is checked through a fresh fork rather than by evaluating in
    // `parent` directly: evaluating in the parent would *itself* write to the
    // shared frame, so a pass would not distinguish "a did not leak" from
    // "the lookup happened before the leak".
    let mut c = parent.fork();
    let in_c = eval(&mut c, "only-in-a");
    assert!(
        matches!(in_c, Err(EvalError::UnboundSymbol { .. })),
        "a later fork of the same parent must not see it either, got {in_c:?}"
    );
}

/// The mutable direction. `set!` against an inherited binding is refused — the
/// binding belongs to the parent and to every sibling, so writing it would be
/// observable outside this child.
///
/// This is the gate a plain `Clone` fails: `Env::set` takes `&self` and mutates
/// through the `Arc`, so cloning shares globals for writes too.
#[test]
fn a_child_cannot_set_a_binding_it_inherited() {
    let mut parent = prototype();
    eval(&mut parent, "(define shared-counter 0)").expect("parent define");

    let mut a = parent.fork();
    let mut b = parent.fork();

    let refused = eval(&mut a, "(set! shared-counter 99)");
    match refused {
        Err(EvalError::SetSealed { ref name, seal, .. }) => {
            assert_eq!(&**name, "shared-counter");
            assert_eq!(seal, Seal::Fork, "and it must say WHICH boundary was hit");
        }
        other => panic!("expected a sealed-write refusal, got {other:?}"),
    }

    // The refusal is not advisory — nothing moved.
    let from_sibling = eval(&mut b, "shared-counter").expect("still bound");
    assert_eq!(int(from_sibling), 0, "the write must not have landed");
}

/// Shadowing is the sanctioned way to get a mutable copy: `define` in the
/// child's own frame, then `set!` that. Without this the seal would make
/// forked interpreters useless for anything stateful.
#[test]
fn a_child_may_shadow_an_inherited_binding_and_mutate_the_shadow() {
    let mut parent = prototype();
    eval(&mut parent, "(define shared-counter 0)").expect("parent define");

    let mut a = parent.fork();
    let mut b = parent.fork();

    eval(&mut a, "(define shared-counter 1)").expect("shadow it");
    eval(&mut a, "(set! shared-counter 42)").expect("the shadow IS writable");
    assert_eq!(int(eval(&mut a, "shared-counter").expect("read")), 42);

    assert_eq!(
        int(eval(&mut b, "shared-counter").expect("read")),
        0,
        "the sibling still sees the parent's value"
    );
}

/// The refusal wording is chosen by the seal, not by the raise site. A forked
/// child must not be told its code is a macro body — that sentence was
/// hard-coded when macro expansion was the only caller that sealed, and it
/// sends the reader to inspect something that isn't there.
#[test]
fn the_refusal_is_worded_for_the_boundary_that_was_actually_crossed() {
    let mut parent = prototype();
    eval(&mut parent, "(define shared 0)").expect("define");
    let mut child = parent.fork();

    let err = eval(&mut child, "(set! shared 1)").expect_err("refused");
    let msg = err.to_string();

    assert!(
        msg.contains("forked") || msg.contains("parent environment"),
        "must name the fork boundary: {msg}"
    );
    assert!(
        !msg.contains("macro"),
        "must NOT claim this is a macro body: {msg}"
    );
}

// ── the floor itself ───────────────────────────────────────────────────────

/// A child cannot pop away its own writable frame. If it could, `define` —
/// which writes to `frames.last()` — would silently start writing into a
/// sealed frame, mutating the parent's globals through the shared `Arc` and
/// defeating every gate above. Unbalanced push/pop is a caller bug; this is
/// the point at which that bug would stop being detectable.
#[test]
fn popping_cannot_take_a_child_below_its_write_floor() {
    use tatara_lisp_eval::Env;

    let parent = Env::new();
    parent.define("inherited", Value::Int(1));

    let mut child = parent.sealed_below_top(Seal::Fork);
    let floor_depth = child.frame_depth();

    for _ in 0..10 {
        child.pop();
    }
    assert_eq!(
        child.frame_depth(),
        floor_depth,
        "the writable frame must survive"
    );

    // And the seal still holds after the attempt.
    child.define("mine", Value::Int(2));
    assert!(
        !child.set("inherited", Value::Int(99)),
        "the inherited binding is still unwritable"
    );
    assert!(
        matches!(parent.lookup("inherited"), Some(Value::Int(1))),
        "and the parent is untouched"
    );
}

/// An unsealed environment keeps exactly its previous pop behaviour. The floor
/// guard is an addition, not a change — without this the guard could tighten
/// ordinary `let` scoping and nothing would say so.
#[test]
fn the_floor_guard_does_not_change_an_unsealed_environment() {
    use tatara_lisp_eval::Env;

    let mut env = Env::new();
    assert_eq!(env.write_floor(), 0);
    env.push();
    env.push();
    assert_eq!(env.frame_depth(), 3);
    env.pop();
    env.pop();
    assert_eq!(env.frame_depth(), 1, "pops down to the root");
    env.pop();
    assert_eq!(env.frame_depth(), 1, "and stops there, as before");
}

// ── the two consumers this was built for ───────────────────────────────────

/// The test-runner shape: a preamble evaluated once, then N isolated bodies.
/// Each body sees the preamble; none sees another's leftovers.
#[test]
fn a_preamble_is_evaluated_once_and_every_body_gets_a_clean_slate() {
    let mut proto = prototype();
    eval(&mut proto, "(define (double x) (* x 2))").expect("preamble");

    for i in 0..3 {
        let mut body = proto.fork();
        // Each body defines the SAME name. Under a shared interpreter the
        // second iteration would observe the first's value.
        assert!(
            eval(&mut body, "per-test").is_err(),
            "iteration {i} started dirty"
        );
        eval(&mut body, "(define per-test (double 21))").expect("body");
        assert_eq!(int(eval(&mut body, "per-test").expect("read")), 42);
    }
}

/// The supervisor shape: a restart is a fresh fork, and it must not inherit
/// the state of the incarnation that died.
#[test]
fn a_restart_does_not_inherit_the_dead_incarnations_state() {
    let proto = prototype();

    let mut first = proto.fork();
    eval(&mut first, "(define crashed-with 'poison)").expect("first incarnation");
    drop(first);

    let mut restarted = proto.fork();
    assert!(
        eval(&mut restarted, "crashed-with").is_err(),
        "the restart must start clean"
    );
}
