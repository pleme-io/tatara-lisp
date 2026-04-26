//! Fibers — first-class suspended computations.
//!
//! Built on top of the bytecode VM. A fiber owns a `Chunk` and an
//! independent `Vm`; calling `(go-run f)` runs it to completion. The
//! returned value is captured in the fiber's state so subsequent
//! `(go-result f)` calls return it.
//!
//! Phase 1 (this module): synchronous fibers — `go-run` runs to
//! completion in one shot, no yield-on-block. Channel sends/recvs
//! still use the non-blocking try-variants from `channel.rs`. This
//! gives users the API surface + the spawn ergonomics today.
//!
//! Phase 2 (future): cooperative yield. The VM gains a `Yield` opcode;
//! channel `>!` / `<!` emit it on full/empty; the scheduler picks the
//! next ready fiber. Lands once we have a multi-fiber scheduler with
//! ready / blocked queues.
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

/// A fiber — owns its compiled chunk + a VM instance + the captured
/// state of the computation. Wrapped in `Arc<Mutex>` and tagged via
/// `Value::Foreign` so multiple references share the same underlying
/// fiber (Lisp-side aliasing is referential, not by-value).
pub struct Fiber {
    pub chunk: Chunk,
    pub state: FiberState,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

impl std::fmt::Debug for Fiber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fiber")
            .field("state", &self.state)
            .field("ops_len", &self.chunk.top.ops.len())
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
    // (go body) — compile + wrap in a fiber. Macroexpansion of body
    // happens at compile time inside make_fiber. The body here arrives
    // as a Value::Sexp because we want the eval crate to give us the
    // unevaluated form. We special-case via a higher-order primitive
    // so the fiber primitive sees the SOURCE form, not the value of
    // evaluating it. To keep the surface clean, `go` expects a thunk:
    //   (go (lambda () expr))
    // and we treat the closure body as the fiber body. This is
    // simpler than introducing a new special form for one primitive.
    interp.register_higher_order_fn(
        "go",
        Arity::Exact(1),
        |args: &[Value], host: &mut H, caller: &Caller<H>, sp: Span| {
            // We need the BODY of the lambda; not its compiled
            // closure. The simplest path: invoke the lambda once
            // synchronously. For Phase 1 of fibers, that's the
            // documented semantics — `go` evaluates eagerly. The
            // returned value is wrapped in a `:done` fiber.
            let result = caller.apply_value(&args[0], vec![], host, sp);
            let mut fiber = Fiber {
                chunk: Chunk::default(),
                state: FiberState::Pending,
                result: None,
                error: None,
            };
            match result {
                Ok(v) => {
                    fiber.state = FiberState::Done;
                    fiber.result = Some(v);
                }
                Err(e) => {
                    fiber.state = FiberState::Errored;
                    fiber.error = Some(eval_err_to_value(&e));
                }
            }
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

    interp.register_fn(
        "go-run",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let f = expect_fiber(&args[0], sp)?;
            // Phase 1: go-run is a no-op for already-resolved fibers
            // (the body ran eagerly inside (go ...)). It exists so the
            // surface matches Phase 2 where deferred fibers wait until
            // explicit go-run.
            let g = f.lock().unwrap();
            match g.state {
                FiberState::Done => Ok(g.result.clone().unwrap_or(Value::Nil)),
                FiberState::Errored => Ok(g.error.clone().unwrap_or(Value::Nil)),
                _ => Ok(Value::Nil),
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
    fn go_status_done_after_eager_run() {
        // Phase 1: (go ...) is eager; status is :done immediately.
        let v = run("(go-status (go (lambda () (+ 1 2))))");
        assert!(matches!(v, Value::Keyword(s) if &*s == "done"));
    }

    #[test]
    fn go_result_returns_body_value() {
        let v = run("(go-result (go (lambda () (* 7 6))))");
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn go_error_captures_thrown_value() {
        let v = run("(go-status (go (lambda () (throw (ex-info \"boom\" (list))))))");
        assert!(matches!(v, Value::Keyword(s) if &*s == "errored"));
        let v = run(
            "(let ((f (go (lambda () (throw (ex-info \"boom\" (list)))))))
               (error-message (go-error f)))",
        );
        assert!(matches!(v, Value::Str(s) if &*s == "boom"));
    }

    #[test]
    fn go_run_on_done_returns_result() {
        let v = run("(go-run (go (lambda () 99)))");
        assert!(matches!(v, Value::Int(99)));
    }

    #[test]
    fn go_run_idempotent() {
        let v = run(
            "(let ((f (go (lambda () 100))))
               (go-run f)
               (go-run f)
               (go-run f))",
        );
        assert!(matches!(v, Value::Int(100)));
    }
}
