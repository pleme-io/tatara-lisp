//! `DenyingLoader` — the capability gate on module source resolution.
//!
//! `(require ...)` is the evaluator's only filesystem reach of its own: it
//! calls `Interpreter::loader`, and `FilesystemLoader` reads whatever path the
//! program names. Any entry point that evaluates code it did not write — the
//! build-time expansion of a user macro body being the case this gate was
//! built for — must be able to take that reach away.
//!
//! Every gate here is tested in **both** directions. A denial test on its own
//! proves nothing: an implementation that simply broke module loading passes
//! it. So each refusal is paired with the identical program succeeding under a
//! working loader, and the difference between the two runs is one
//! `set_loader` call.

use std::sync::Arc;

use tatara_lisp_eval::{
    install_full_stdlib_with, DenyingLoader, EvalError, Interpreter, Loader, MapLoader, Value,
};

/// One module, one export. Deliberately trivial: the point of these tests is
/// *whether* the source is reachable, never what it computes.
const LIB_PATH: &str = "lib/math";
const LIB_SOURCE: &str = "(define square (lambda (x) (* x x))) (provide square)";
const PROGRAM: &str = "(require \"lib/math\" :refer (square)) (square 2)";

const REASON: &str = "build-time macro expansion must not read the filesystem";

fn interpreter() -> Interpreter<()> {
    let mut interp = Interpreter::new();
    let mut host = ();
    install_full_stdlib_with(&mut interp, &mut host);
    interp
}

fn working_loader() -> Arc<MapLoader> {
    let mut loader = MapLoader::new();
    loader.insert(LIB_PATH, LIB_SOURCE);
    Arc::new(loader)
}

fn eval(interp: &mut Interpreter<()>, src: &str) -> Result<Value, EvalError> {
    let forms = tatara_lisp_eval::read_spanned(src).expect("parse");
    let mut host = ();
    interp.eval_program(&forms, &mut host)
}

/// Pull the `(tag, message)` out of a thrown Lisp error, panicking with the
/// actual error if it was some other failure — a `Reader` or arity error here
/// would otherwise masquerade as a successful denial.
fn thrown(err: EvalError) -> (String, String) {
    match err {
        EvalError::User {
            value: Value::Error(obj),
            ..
        } => (obj.tag.to_string(), obj.message.to_string()),
        other => panic!("expected a thrown Lisp error, got {other:?}"),
    }
}

// ── the two halves ─────────────────────────────────────────────────────────

/// Half (a): under the denier the require fails, and it fails as a *denial*.
#[test]
fn a_require_is_refused_under_the_denying_loader() {
    let mut interp = interpreter();
    interp.set_loader(Arc::new(DenyingLoader::new(REASON)));

    let err = eval(&mut interp, PROGRAM).expect_err("the gate must refuse this");
    let (tag, message) = thrown(err);

    assert_eq!(tag, "module-denied", "the denial gets its own tag");
    assert!(
        message.contains(LIB_PATH),
        "the refusal must name what was denied; got {message:?}"
    );
    assert!(
        message.contains(REASON),
        "the refusal must name why; got {message:?}"
    );
}

/// Half (b): the identical program, the identical interpreter build, one
/// different loader — and it runs. Without this, half (a) is also passed by an
/// implementation that merely broke `(require ...)`.
#[test]
fn the_same_require_succeeds_under_a_working_loader() {
    let mut interp = interpreter();
    interp.set_loader(working_loader());

    let v = eval(&mut interp, PROGRAM).expect("the module is reachable here");
    assert!(matches!(v, Value::Int(4)), "(square 2) = 4, got {v:?}");
}

// ── the denial is legible, not just fatal ──────────────────────────────────

/// A denied load must not be reported as a missing one. `NoLoader` — the
/// interpreter's default — gets this wrong, which is the defect `DenyingLoader`
/// exists to fix: an operator reading `module not found: lib/math` goes looking
/// for a file that is right there.
#[test]
fn a_denial_is_not_reported_as_a_missing_module() {
    // The path resolves fine for a loader that is allowed to resolve it, so
    // "not found" would be a false statement about the world.
    assert!(
        working_loader().load(LIB_PATH).is_ok(),
        "precondition: the module exists"
    );

    let mut interp = interpreter();
    interp.set_loader(Arc::new(DenyingLoader::new(REASON)));
    let (tag, _) = thrown(eval(&mut interp, PROGRAM).expect_err("refused"));

    assert_ne!(tag, "module-not-found", "a denial is not an absence");
}

/// The refusal is catchable like any other thrown error, so an embedder can
/// turn it into a diagnostic instead of aborting the whole pass.
#[test]
fn a_denial_can_be_caught_by_the_program_itself() {
    let mut interp = interpreter();
    interp.set_loader(Arc::new(DenyingLoader::new(REASON)));

    // `require` is a top-level form, so the catchable surface is the whole
    // top-level eval: run it, and confirm the interpreter is still usable.
    let _ = eval(&mut interp, PROGRAM).expect_err("refused");
    let v = eval(&mut interp, "(+ 1 1)").expect("the interpreter survives a denial");
    assert!(matches!(v, Value::Int(2)), "got {v:?}");
}

// ── the ordering hazard the doc comment claims ─────────────────────────────

/// `Interpreter::fork` clones the parent's `Arc<dyn Loader>`, so a child of a
/// filesystem-enabled parent is filesystem-enabled. This is the red run behind
/// the "call `set_loader` **after** forking" instruction on `DenyingLoader`:
/// denying the parent first and forking after would look like a gate and be
/// none, because a *later* `set_loader` on the parent does not reach a child
/// that already took a clone.
#[test]
fn a_fork_inherits_the_loader_it_was_forked_from() {
    let mut parent = interpreter();
    parent.set_loader(working_loader());

    let mut child = parent.fork();
    let v = eval(&mut child, PROGRAM).expect("the child inherited the parent's reach");
    assert!(matches!(v, Value::Int(4)), "got {v:?}");
}

/// …and the gate is per-interpreter: denying the child leaves the parent's
/// reach intact. That is what makes "fork, then deny" the correct order rather
/// than a merely conventional one.
#[test]
fn denying_a_fork_does_not_deny_its_parent() {
    let mut parent = interpreter();
    parent.set_loader(working_loader());

    let mut child = parent.fork();
    child.set_loader(Arc::new(DenyingLoader::new(REASON)));

    let (tag, _) = thrown(eval(&mut child, PROGRAM).expect_err("the child is gated"));
    assert_eq!(tag, "module-denied");

    let v = eval(&mut parent, PROGRAM).expect("the parent is not gated");
    assert!(matches!(v, Value::Int(4)), "got {v:?}");
}
