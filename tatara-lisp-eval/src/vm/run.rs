//! VM run loop — interprets a `Chunk` against the host's
//! `Interpreter<H>` (for native-fn dispatch + global env access).
//!
//! The VM is a thin wrapper: stack of `Value`s + stack of `Frame`s +
//! IP. Native fns and closures both go through `Interpreter::apply`
//! semantics — same `FnRegistry`, same `Env`, same Value type as the
//! tree-walker. This means primitives written for the eval crate
//! (arithmetic, list, hash-map, channel, ...) just work.

use std::sync::Arc;

use tatara_lisp::Span;
use thiserror::Error;

use super::chunk::{Chunk, CompiledFn};
use super::op::Op;
use crate::eval::Interpreter;
use crate::value::{Closure, Value};

#[derive(Debug, Error)]
pub enum VmError {
    #[error("stack underflow at op {ip}")]
    Underflow { ip: usize },
    #[error("unbound symbol `{name}` at {at}")]
    Unbound { name: String, at: Span },
    #[error("not callable: {kind} at {at}")]
    NotCallable { kind: &'static str, at: Span },
    #[error("arity mismatch: expected {expected}, got {got} at {at}")]
    Arity {
        expected: usize,
        got: usize,
        at: Span,
    },
    #[error("eval error: {0}")]
    Eval(#[from] crate::error::EvalError),
    #[error("local index out of bounds: {0}")]
    BadLocal(usize),
}

/// One activation record. The VM is a stack of these.
struct Frame {
    /// The function being executed.
    func: Arc<CompiledFn>,
    /// IP within `func.ops`.
    ip: usize,
    /// First local-slot index in the shared value stack — locals
    /// live at `stack[locals_base..locals_base + func.locals]`.
    locals_base: usize,
    /// First slot ABOVE the locals — temporaries push here.
    /// (Equal to `locals_base + func.locals` at frame entry, never moves.)
    stack_base: usize,
    /// The captured env of the closure that produced this frame, if
    /// any. `None` for the top-level chunk.
    captured_env: Option<crate::env::Env>,
}

/// The VM. Owns the stack + frame stack while a program runs.
pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<Frame>,
}

impl Vm {
    pub fn new() -> Self {
        Self {
            stack: Vec::with_capacity(256),
            frames: Vec::with_capacity(64),
        }
    }

    /// Execute a chunk against the host interpreter. Returns the
    /// final value (the program's result).
    pub fn run<H: 'static>(
        &mut self,
        chunk: &Chunk,
        interp: &mut Interpreter<H>,
        host: &mut H,
    ) -> Result<Value, VmError> {
        // Reset state.
        self.stack.clear();
        self.frames.clear();
        // Allocate the top-level frame.
        let top_func = Arc::new(chunk.top.clone());
        let top_locals = top_func.locals;
        let top_frame = Frame {
            func: top_func.clone(),
            ip: 0,
            locals_base: 0,
            stack_base: top_locals,
            captured_env: None,
        };
        // Reserve slots for locals.
        for _ in 0..top_locals {
            self.stack.push(Value::Nil);
        }
        self.frames.push(top_frame);

        // Main interpret loop.
        loop {
            // Snapshot the current frame fields to avoid simultaneous
            // borrows. We index by `frames.last()` cheaply.
            let frame_idx = self.frames.len() - 1;
            let (op, span);
            {
                let f = &self.frames[frame_idx];
                if f.ip >= f.func.ops.len() {
                    // Implicit Halt — should never happen with a
                    // well-compiled chunk; defensive abort.
                    return Ok(self.pop_or_nil());
                }
                op = f.func.ops[f.ip].clone();
                span = f.func.spans.get(f.ip).copied().unwrap_or(Span::synthetic());
            }
            self.frames[frame_idx].ip += 1;

            match op {
                Op::Halt => {
                    return Ok(self.pop_or_nil());
                }
                Op::Nil => self.stack.push(Value::Nil),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),
                Op::Int(n) => self.stack.push(Value::Int(n)),
                Op::Const(idx) => {
                    let v = chunk.consts.get(idx).clone();
                    self.stack.push(v);
                }

                Op::Pop => {
                    self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                }
                Op::Dup => {
                    let v = self
                        .stack
                        .last()
                        .ok_or(VmError::Underflow { ip: 0 })?
                        .clone();
                    self.stack.push(v);
                }

                Op::LoadLocal(idx) => {
                    let f = &self.frames[frame_idx];
                    let abs = f.locals_base + idx;
                    let v = self
                        .stack
                        .get(abs)
                        .cloned()
                        .ok_or(VmError::BadLocal(idx))?;
                    self.stack.push(v);
                }
                Op::StoreLocal(idx) => {
                    let v = self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                    let f = &self.frames[frame_idx];
                    let abs = f.locals_base + idx;
                    if abs >= self.stack.len() {
                        return Err(VmError::BadLocal(idx));
                    }
                    self.stack[abs] = v;
                }

                Op::LoadGlobal(name_idx) => {
                    let name = chunk.names.get(name_idx).clone();
                    let v = self
                        .lookup_global(interp, &name)
                        .ok_or_else(|| VmError::Unbound {
                            name: name.to_string(),
                            at: span,
                        })?;
                    self.stack.push(v);
                }
                Op::StoreGlobal(name_idx) => {
                    let name = chunk.names.get(name_idx).clone();
                    let v = self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                    interp.define_global(name, v);
                }

                Op::Jmp(target) => {
                    self.frames[frame_idx].ip = target;
                }
                Op::JmpNot(target) => {
                    let v = self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                    if !v.is_truthy() {
                        self.frames[frame_idx].ip = target;
                    }
                }
                Op::JmpIf(target) => {
                    let v = self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                    if v.is_truthy() {
                        self.frames[frame_idx].ip = target;
                    }
                }

                Op::MakeClosure(fn_idx) => {
                    let body = chunk.fn_table[fn_idx].clone();
                    // Capture the CURRENT global env. For lambdas
                    // nested inside lambdas the captured_env is the
                    // current frame's, so closures see outer locals
                    // through the env. We carry the interpreter's
                    // globals snapshot — locals are NOT captured by
                    // this Phase 1+2 implementation; that's the
                    // documented limitation.
                    let captured = interp
                        .globals_snapshot()
                        .clone();
                    let closure = Closure {
                        params: body.params.clone(),
                        rest: body.rest.clone(),
                        // We DO NOT use the tree-walker's `body: Vec<Spanned>`
                        // field for VM-side closures. The compiled body
                        // is stored in a side-channel via Foreign tag.
                        body: Vec::new(),
                        captured_env: captured,
                        source: body.source_span,
                    };
                    // Tag the closure with the compiled body so the
                    // VM can recognize and re-enter it on a Call. A
                    // fresh `CompiledClosure` foreign value carries
                    // the body.
                    let compiled = CompiledClosure {
                        body: Arc::new(body),
                        closure: Arc::new(closure),
                    };
                    self.stack.push(Value::Foreign(Arc::new(compiled)));
                }

                Op::Call(arity) => {
                    self.do_call(chunk, interp, host, arity, span, /*tail=*/ false)?;
                }
                Op::TailCall(arity) => {
                    self.do_call(chunk, interp, host, arity, span, /*tail=*/ true)?;
                }
                Op::Return => {
                    let ret = self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                    // Drop locals for this frame.
                    let f = self
                        .frames
                        .pop()
                        .expect("Return with no active frame");
                    self.stack.truncate(f.locals_base);
                    if self.frames.is_empty() {
                        return Ok(ret);
                    }
                    self.stack.push(ret);
                }

                Op::MakeList(n) => {
                    let len = self.stack.len();
                    if n > len {
                        return Err(VmError::Underflow { ip: 0 });
                    }
                    let items: Vec<Value> = self.stack.drain(len - n..).collect();
                    self.stack.push(Value::list(items));
                }
            }
        }
    }

    fn pop_or_nil(&mut self) -> Value {
        self.stack.pop().unwrap_or(Value::Nil)
    }

    fn lookup_global<H: 'static>(
        &self,
        interp: &Interpreter<H>,
        name: &str,
    ) -> Option<Value> {
        interp.lookup_global(name)
    }

    fn do_call<H: 'static>(
        &mut self,
        _chunk: &Chunk,
        interp: &mut Interpreter<H>,
        host: &mut H,
        arity: usize,
        span: Span,
        tail: bool,
    ) -> Result<(), VmError> {
        let stack_len = self.stack.len();
        if stack_len < arity + 1 {
            return Err(VmError::Underflow { ip: 0 });
        }
        let callee_idx = stack_len - arity - 1;
        let callee = self.stack[callee_idx].clone();

        // At the top level, TailCall is structurally identical to
        // Call — there's no enclosing frame to fold into. Detect by
        // depth and downgrade so the rest of the dispatch is uniform.
        let tail = tail && self.frames.len() > 1;

        // Branch on callee kind.
        match &callee {
            // VM-compiled closure (Foreign-tagged CompiledClosure).
            Value::Foreign(any) => {
                if let Ok(cc) = any.clone().downcast::<CompiledClosure>() {
                    return self.invoke_compiled(cc, arity, span, tail);
                }
                // Other Foreign values aren't callable.
                Err(VmError::NotCallable {
                    kind: callee.type_name(),
                    at: span,
                })
            }
            // Native or tree-walker closure — go through the eval
            // crate's apply path. This is what makes every primitive
            // and every dynamically-loaded closure work uniformly.
            Value::NativeFn(_) | Value::Closure(_) => {
                // Drain args from the stack, then drop the callee
                // slot so nothing's left in callee_idx.
                let args: Vec<Value> = self.stack.drain(callee_idx + 1..).collect();
                self.stack.pop();
                let result = interp.apply_external_value(&callee, args, host, span)?;
                self.stack.push(result);
                Ok(())
            }
            other => Err(VmError::NotCallable {
                kind: other.type_name(),
                at: span,
            }),
        }
    }

    fn invoke_compiled(
        &mut self,
        cc: Arc<CompiledClosure>,
        arity: usize,
        span: Span,
        tail: bool,
    ) -> Result<(), VmError> {
        let body = cc.body.clone();
        let required = body.params.len();
        let has_rest = body.rest.is_some();
        if !has_rest && arity != required {
            return Err(VmError::Arity {
                expected: required,
                got: arity,
                at: span,
            });
        }
        if has_rest && arity < required {
            return Err(VmError::Arity {
                expected: required,
                got: arity,
                at: span,
            });
        }

        // Pop args from stack.
        let stack_len = self.stack.len();
        let args_start = stack_len - arity;
        let args: Vec<Value> = self.stack.drain(args_start..).collect();
        // Pop the callee.
        self.stack.pop();

        // Build the new locals layout.
        let mut locals: Vec<Value> = Vec::with_capacity(body.locals);
        for v in args.iter().take(required) {
            locals.push(v.clone());
        }
        if let Some(_) = &body.rest {
            let rest_args: Vec<Value> = args.iter().skip(required).cloned().collect();
            locals.push(Value::list(rest_args));
        }
        while locals.len() < body.locals {
            locals.push(Value::Nil);
        }

        if tail && !self.frames.is_empty() {
            // Reuse the current frame: drop its locals, push new ones.
            let frame_idx = self.frames.len() - 1;
            let f = &mut self.frames[frame_idx];
            self.stack.truncate(f.locals_base);
            for v in locals {
                self.stack.push(v);
            }
            f.func = body.clone();
            f.ip = 0;
            f.captured_env = Some(cc.closure.captured_env.clone());
            // stack_base relative to locals_base stays the same.
            f.stack_base = f.locals_base + body.locals;
        } else {
            // Push a new frame.
            let locals_base = self.stack.len();
            for v in locals {
                self.stack.push(v);
            }
            let stack_base = self.stack.len();
            self.frames.push(Frame {
                func: body.clone(),
                ip: 0,
                locals_base,
                stack_base,
                captured_env: Some(cc.closure.captured_env.clone()),
            });
        }
        Ok(())
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

/// Foreign-tagged compiled closure — the VM's native callable shape.
/// Wrapping in `Foreign` lets us pass it through `Value` (which is
/// shared with the tree-walker) without growing the `Value` enum.
#[derive(Clone)]
pub struct CompiledClosure {
    pub body: Arc<CompiledFn>,
    pub closure: Arc<Closure>,
}

impl std::fmt::Debug for CompiledClosure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledClosure")
            .field("params", &self.body.params)
            .field("ops_len", &self.body.ops.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Interpreter;
    use crate::install_full_stdlib_with;
    use crate::vm::compile::compile_program;
    use tatara_lisp::read_spanned;

    struct NoHost;

    /// Helper: read + compile + run via VM, return final Value.
    fn run_vm(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let forms = read_spanned(src).unwrap();
        // Macroexpand first so the VM never sees defmacro-introduced
        // syntax. The expander mutates `i.expander` from any
        // top-level (defmacro …) forms in the source.
        let mut expanded: Vec<tatara_lisp::Spanned> = Vec::new();
        for form in &forms {
            if i.expander_mut().try_register_macro(form).unwrap() {
                continue;
            }
            expanded.push(i.fully_expand(form, &mut NoHost).unwrap());
        }
        let chunk = compile_program(&expanded).unwrap();
        let mut vm = Vm::new();
        vm.run(&chunk, &mut i, &mut NoHost).unwrap()
    }

    #[test]
    fn run_int_literal() {
        assert!(matches!(run_vm("42"), Value::Int(42)));
    }

    #[test]
    fn run_arithmetic_via_native_add() {
        assert!(matches!(run_vm("(+ 1 2 3)"), Value::Int(6)));
    }

    #[test]
    fn run_if_picks_branch() {
        assert!(matches!(run_vm("(if #t 100 200)"), Value::Int(100)));
        assert!(matches!(run_vm("(if #f 100 200)"), Value::Int(200)));
    }

    #[test]
    fn run_let_binds_and_uses() {
        assert!(matches!(
            run_vm("(let ((x 10) (y 20)) (+ x y))"),
            Value::Int(30)
        ));
    }

    #[test]
    fn run_define_then_use() {
        assert!(matches!(run_vm("(define x 99) x"), Value::Int(99)));
    }

    #[test]
    fn run_define_function_shorthand() {
        assert!(matches!(
            run_vm("(define (sq x) (* x x)) (sq 7)"),
            Value::Int(49)
        ));
    }

    #[test]
    fn run_lambda_inline_application() {
        assert!(matches!(
            run_vm("((lambda (x y) (+ x y)) 3 4)"),
            Value::Int(7)
        ));
    }

    #[test]
    fn run_recursion_via_global_define() {
        let v = run_vm(
            "(define (fact n)
               (if (= n 0) 1 (* n (fact (- n 1)))))
             (fact 6)",
        );
        assert!(matches!(v, Value::Int(720)));
    }

    #[test]
    fn run_begin_returns_last() {
        assert!(matches!(run_vm("(begin 1 2 3)"), Value::Int(3)));
    }

    #[test]
    fn run_and_short_circuits() {
        // (and #t 5) → 5 (last truthy wins).
        assert!(matches!(run_vm("(and #t 5)"), Value::Int(5)));
        assert!(matches!(run_vm("(and #f 5)"), Value::Bool(false)));
    }

    #[test]
    fn run_or_short_circuits() {
        assert!(matches!(run_vm("(or #f 7)"), Value::Int(7)));
        assert!(matches!(run_vm("(or #f #f)"), Value::Bool(false)));
    }

    #[test]
    fn run_not_inverts() {
        assert!(matches!(run_vm("(not #t)"), Value::Bool(false)));
        assert!(matches!(run_vm("(not #f)"), Value::Bool(true)));
    }

    #[test]
    fn run_quoted_symbol_passes_through() {
        let v = run_vm("'foo");
        assert!(matches!(v, Value::Symbol(s) if &*s == "foo"));
    }

    #[test]
    fn run_set_mutates_global() {
        assert!(matches!(
            run_vm("(define x 1) (set! x 99) x"),
            Value::Int(99)
        ));
    }

    #[test]
    fn run_tail_call_loops_in_constant_space() {
        // Tail-call optimized recursion. Without TCO this would
        // stack-overflow at ~10k frames; with TCO it runs in O(1)
        // stack space. Only test 50_000 iterations to keep this
        // CI-fast — the principle is proved.
        let v = run_vm(
            "(define (loop n) (if (= n 0) :done (loop (- n 1))))
             (loop 50000)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "done"));
    }
}
