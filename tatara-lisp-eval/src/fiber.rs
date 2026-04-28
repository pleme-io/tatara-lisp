//! Fibers — first-class suspended computations.
//!
//! Built on top of the bytecode VM. A fiber owns a `Chunk` and an
//! independent `Vm`; calling `(go-run f)` runs it to completion. The
//! returned value is captured in the fiber's state so subsequent
//! `(go-result f)` calls return it.
//!
//! Phase 2 (this module): deferred fibers — `(go thunk)` returns a
//! :pending fiber holding an un-invoked thunk; the body runs only
//! when `(go-run f)` is called. Repeated `go-run` is idempotent.
//! Channel sends/recvs still use the non-blocking try-variants from
//! `channel.rs`.
//!
//! Phase 3 (future): true cooperative yield. The VM gains a `Yield`
//! opcode; channel `>!` / `<!` emit it on full/empty; a scheduler
//! drives a fiber set round-robin. Lands once we have a multi-fiber
//! scheduler with ready / blocked queues. The deferred shape from
//! Phase 2 is forward-compatible — `(go thunk)` already returns a
//! pending fiber the scheduler can park.
//!
//! Surface (registered by `install_fibers`):
//!
//! ```text
//!   (go body)          → fiber handle
//!   (go? v)            → bool
//!   (go-run f)         → runs the body to completion; sets state to :done
//!   (go-status f)      → :pending | :running | :done | :error
//!   (go-result f)      → captured value or nil before run
//!   (go-error f)       → captured Value::Error or nil
//! ```

use std::sync::{Arc, Mutex};

use tatara_lisp::{Atom, Span, Spanned, SpannedForm};

use crate::error::{EvalError, Result};
use crate::eval::Interpreter;
use crate::ffi::{Arity, Caller};
use crate::value::Value;
use crate::vm::{Chunk, Vm, compile_program};

/// One fiber's state.
#[derive(Debug, Clone, PartialEq)]
pub enum FiberState {
    Pending,
    Running,
    Done,
    Errored,
}

/// A fiber — wraps a deferred 0-arg callable plus its result/error
/// state. Wrapped in `Arc<Mutex>` and tagged via `Value::Foreign` so
/// multiple references share the same underlying fiber (Lisp-side
/// aliasing is referential, not by-value).
///
/// `thunk` is a 0-arg callable Value (Closure / NativeFn / VM
/// CompiledClosure). It's invoked at most once — by the first
/// `go-run` — and the result/error replaces it (set to `None` so we
/// don't keep the closure alive longer than needed).
pub struct Fiber {
    /// Optional pre-compiled chunk — kept for forward compatibility
    /// with the Phase 3 scheduler that owns its own VM instance per
    /// fiber. Phase 2 paths typically leave it as `Chunk::default()`.
    pub chunk: Chunk,
    /// Deferred thunk to invoke on first `go-run`. `None` once the
    /// fiber has resolved (Done / Errored).
    pub thunk: Option<Value>,
    pub state: FiberState,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

impl std::fmt::Debug for Fiber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fiber")
            .field("state", &self.state)
            .field("ops_len", &self.chunk.top.ops.len())
            .field("has_thunk", &self.thunk.is_some())
            .field("has_result", &self.result.is_some())
            .finish()
    }
}

/// Construct a fiber wrapping a single body form. The body is
/// compiled (after macro expansion) into a 0-arg chunk that captures
/// no locals — globals are still visible at run time via the host
/// interpreter that runs the fiber.
pub fn make_fiber<H: 'static>(
    body: &Spanned,
    interp: &mut Interpreter<H>,
    host: &mut H,
) -> Result<Value> {
    let expanded = interp.fully_expand(body, host)?;
    let chunk = compile_program(std::slice::from_ref(&expanded)).map_err(|e| match e {
        crate::vm::CompileError::Bad { at, message } => EvalError::bad_form(
            Arc::<str>::from("go"),
            format!("compile body: {message}"),
            at,
        ),
    })?;
    let fiber = Fiber {
        chunk,
        thunk: None,
        state: FiberState::Pending,
        result: None,
        error: None,
    };
    Ok(Value::Foreign(Arc::new(Mutex::new(fiber))))
}

/// Drive a fiber to completion. Phase 1 semantics: synchronous — runs
/// the entire body in one shot. Repeated `go-run` on a `:done` fiber
/// is a no-op; on `:errored` it returns the cached error.
pub fn run_fiber<H: 'static>(
    fiber: &Arc<Mutex<Fiber>>,
    interp: &mut Interpreter<H>,
    host: &mut H,
) -> Result<Value> {
    // Snapshot what's needed to run, drop the lock during execution.
    let chunk = {
        let f = fiber.lock().unwrap();
        match f.state {
            FiberState::Done => return Ok(f.result.clone().unwrap_or(Value::Nil)),
            FiberState::Errored => return Ok(f.error.clone().unwrap_or(Value::Nil)),
            FiberState::Running => {
                return Err(EvalError::native_fn(
                    Arc::<str>::from("go-run"),
                    "fiber already running (re-entrant call detected)",
                    Span::synthetic(),
                ));
            }
            _ => {}
        }
        f.chunk.clone()
    };
    {
        let mut f = fiber.lock().unwrap();
        f.state = FiberState::Running;
    }
    let mut vm = Vm::new();
    let result = vm.run(&chunk, interp, host);
    let mut f = fiber.lock().unwrap();
    match result {
        Ok(v) => {
            f.state = FiberState::Done;
            f.result = Some(v.clone());
            Ok(v)
        }
        Err(e) => {
            f.state = FiberState::Errored;
            // Convert the VmError to a Value::Error so it round-trips
            // through Lisp catch handlers uniformly. User-thrown
            // errors (the inner EvalError::User shape) keep their
            // original Value::Error for transparency.
            let err_value = match e {
                crate::vm::VmError::Eval(inner) => eval_err_to_value(&inner),
                other => Value::Error(Arc::new(crate::value::ErrorObj {
                    tag: Arc::from("fiber-error"),
                    message: Arc::from(format!("{other}")),
                    data: Vec::new(),
                })),
            };
            f.error = Some(err_value.clone());
            Ok(err_value)
        }
    }
}

/// Convert an `EvalError` into a Lisp-side `Value::Error` so it
/// round-trips through Lisp catch handlers / fiber error fields
/// uniformly. User-thrown errors (via `(throw ...)`) preserve the
/// original Value::Error the user constructed; other Rust-side
/// errors get a `:fiber-error` tag with the formatted message.
fn eval_err_to_value(e: &EvalError) -> Value {
    if let EvalError::User { value, .. } = e {
        // User-thrown — pass the original value through verbatim.
        return value.clone();
    }
    Value::Error(Arc::new(crate::value::ErrorObj {
        tag: Arc::from("fiber-error"),
        message: Arc::from(format!("{e}")),
        data: Vec::new(),
    }))
}

fn expect_fiber(v: &Value, sp: Span) -> Result<Arc<Mutex<Fiber>>> {
    match v {
        Value::Foreign(any) => any
            .clone()
            .downcast::<Mutex<Fiber>>()
            .map_err(|_| EvalError::type_mismatch("fiber", v.type_name(), sp)),
        other => Err(EvalError::type_mismatch("fiber", other.type_name(), sp)),
    }
}

/// Names registered by `install_fibers`.
pub const FIBER_NAMES: &[&str] = &[
    "go",
    "go-error",
    "go-result",
    "go-run",
    "go-status",
    "go?",
];

pub fn install_fibers<H: 'static>(interp: &mut Interpreter<H>) {
    // (go thunk) — defer thunk for later invocation. Returns a
    // :pending fiber holding the un-invoked closure. The body runs
    // only when (go-run f) is called.
    //
    // Surface uses a thunk (`(lambda () body)`) so we don't need a
    // special form — any callable Value works. Embedders that want
    // `(go body)` syntax should provide a macro that wraps `body`
    // in `(lambda () body)` before passing.
    interp.register_fn(
        "go",
        Arity::Exact(1),
        |args: &[Value], _host: &mut H, sp| {
            // Validate the arg is callable (closure / nativefn /
            // compiled closure). We don't invoke yet — just stash.
            match &args[0] {
                Value::Closure(_) | Value::NativeFn(_) | Value::Foreign(_) => {}
                other => {
                    return Err(EvalError::type_mismatch("callable", other.type_name(), sp));
                }
            }
            let fiber = Fiber {
                chunk: Chunk::default(),
                thunk: Some(args[0].clone()),
                state: FiberState::Pending,
                result: None,
                error: None,
            };
            Ok(Value::Foreign(Arc::new(Mutex::new(fiber))))
        },
    );

    interp.register_fn(
        "go?",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, _sp| {
            let is = match &args[0] {
                Value::Foreign(any) => any.clone().downcast::<Mutex<Fiber>>().is_ok(),
                _ => false,
            };
            Ok(Value::Bool(is))
        },
    );

    interp.register_fn(
        "go-status",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let f = expect_fiber(&args[0], sp)?;
            let g = f.lock().unwrap();
            let kw = match g.state {
                FiberState::Pending => "pending",
                FiberState::Running => "running",
                FiberState::Done => "done",
                FiberState::Errored => "errored",
            };
            Ok(Value::Keyword(Arc::from(kw)))
        },
    );

    interp.register_fn(
        "go-result",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let f = expect_fiber(&args[0], sp)?;
            let g = f.lock().unwrap();
            Ok(g.result.clone().unwrap_or(Value::Nil))
        },
    );

    interp.register_fn(
        "go-error",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let f = expect_fiber(&args[0], sp)?;
            let g = f.lock().unwrap();
            Ok(g.error.clone().unwrap_or(Value::Nil))
        },
    );

    interp.register_higher_order_fn(
        "go-run",
        Arity::Exact(1),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            let f = expect_fiber(&args[0], sp)?;
            // Snapshot the thunk while holding the lock briefly,
            // then drop the lock so the thunk can run without
            // blocking re-entrant fiber observation.
            let thunk = {
                let mut g = f.lock().unwrap();
                match g.state {
                    FiberState::Done => return Ok(g.result.clone().unwrap_or(Value::Nil)),
                    FiberState::Errored => return Ok(g.error.clone().unwrap_or(Value::Nil)),
                    FiberState::Running => {
                        return Err(EvalError::native_fn(
                            Arc::<str>::from("go-run"),
                            "fiber already running (re-entrant call detected)",
                            sp,
                        ));
                    }
                    FiberState::Pending => {}
                }
                g.state = FiberState::Running;
                g.thunk.take()
            };
            let thunk = match thunk {
                Some(t) => t,
                None => {
                    // Pending state with no thunk — defensive abort.
                    let mut g = f.lock().unwrap();
                    g.state = FiberState::Errored;
                    let v = Value::Error(Arc::new(crate::value::ErrorObj {
                        tag: Arc::from("fiber-corrupt"),
                        message: Arc::from("pending fiber has no thunk"),
                        data: Vec::new(),
                    }));
                    g.error = Some(v.clone());
                    return Ok(v);
                }
            };
            // Drive the thunk through the standard apply path. This
            // works whether the thunk is a tree-walker Closure, a
            // VM-compiled Foreign(CompiledClosure), or a NativeFn.
            let result = caller.apply_value(&thunk, vec![], host, sp);
            let mut g = f.lock().unwrap();
            match result {
                Ok(v) => {
                    g.state = FiberState::Done;
                    g.result = Some(v.clone());
                    Ok(v)
                }
                Err(e) => {
                    g.state = FiberState::Errored;
                    let err_value = eval_err_to_value(&e);
                    g.error = Some(err_value.clone());
                    Ok(err_value)
                }
            }
        },
    );
}

/// Convenience: convert a body Spanned form into a fiber Value.
/// Useful for embedders that want to expose a `(go form)` macro that
/// avoids the lambda-wrapping idiom — they translate `(go body)` into
/// `(go (lambda () body))` at the macro layer, then call this from
/// the eval crate's normal path.
pub fn body_to_fiber<H: 'static>(
    body: &Spanned,
    interp: &mut Interpreter<H>,
    host: &mut H,
) -> Result<Value> {
    // Wrap as `(lambda () body)` so the existing fiber pipeline can
    // invoke it. Same shape as the macro the user would write.
    let lambda = Spanned::new(
        body.span,
        SpannedForm::List(vec![
            Spanned::new(body.span, SpannedForm::Atom(Atom::Symbol("lambda".into()))),
            Spanned::new(body.span, SpannedForm::List(Vec::new())),
            body.clone(),
        ]),
    );
    make_fiber(&lambda, interp, host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_full_stdlib_with;
    use crate::Interpreter;
    use tatara_lisp::read_spanned;

    struct NoHost;

    fn run(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        install_fibers(&mut i);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    #[test]
    fn go_creates_a_fiber_value() {
        let v = run("(go (lambda () 42))");
        // Returned a Value::Foreign carrying a Mutex<Fiber>.
        match v {
            Value::Foreign(any) => {
                assert!(any.downcast::<Mutex<Fiber>>().is_ok());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn go_predicate_distinguishes() {
        assert!(matches!(run("(go? (go (lambda () 1)))"), Value::Bool(true)));
        assert!(matches!(run("(go? 42)"), Value::Bool(false)));
    }

    #[test]
    fn go_status_pending_before_run() {
        // Phase 2: (go ...) is deferred; status is :pending until
        // an explicit go-run drives it.
        let v = run("(go-status (go (lambda () (+ 1 2))))");
        assert!(matches!(v, Value::Keyword(s) if &*s == "pending"));
    }

    #[test]
    fn go_run_drives_to_done() {
        let v = run(
            "(let ((f (go (lambda () (* 7 6)))))
               (go-run f)
               (go-status f))",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "done"));
    }

    #[test]
    fn go_result_after_explicit_run() {
        let v = run(
            "(let ((f (go (lambda () (* 7 6)))))
               (go-run f)
               (go-result f))",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn go_result_nil_before_run() {
        // Pending fiber has no result yet.
        let v = run("(go-result (go (lambda () 999)))");
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn go_error_captures_thrown_value() {
        // Status :pending until run; :errored after run drives the throw.
        let v = run(
            "(let ((f (go (lambda () (throw (ex-info \"boom\" (list)))))))
               (go-run f)
               (go-status f))",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "errored"));
        let v = run(
            "(let ((f (go (lambda () (throw (ex-info \"boom\" (list)))))))
               (go-run f)
               (error-message (go-error f)))",
        );
        assert!(matches!(v, Value::Str(s) if &*s == "boom"));
    }

    #[test]
    fn go_run_returns_body_value() {
        let v = run("(go-run (go (lambda () 99)))");
        assert!(matches!(v, Value::Int(99)));
    }

    #[test]
    fn go_run_idempotent_after_done() {
        let v = run(
            "(let ((f (go (lambda () 100))))
               (go-run f)
               (go-run f)
               (go-run f))",
        );
        assert!(matches!(v, Value::Int(100)));
    }

    #[test]
    fn go_thunk_with_closure_captures() {
        // Thunk closes over a let-local — verifies deferred invocation
        // preserves the closure's captured environment.
        let v = run(
            "(let ((x 21))
               (let ((f (go (lambda () (* x 2)))))
                 (go-run f)))",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn go_rejects_non_callable() {
        // Type guard in (go ...) — passing a non-callable should
        // surface a type-mismatch error rather than building a
        // fiber whose go-run will mysteriously fail later.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        install_fibers(&mut i);
        let forms = read_spanned("(go 42)").unwrap();
        let err = i.eval_program(&forms, &mut NoHost).unwrap_err();
        // Should be a TypeMismatch — wrapped value is non-callable.
        assert!(
            matches!(err, EvalError::TypeMismatch { .. }),
            "expected TypeMismatch, got {err:?}"
        );
    }
}
