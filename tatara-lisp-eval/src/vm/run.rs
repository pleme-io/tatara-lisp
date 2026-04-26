//! VM run loop — interprets a `Chunk` against the host's
//! `Interpreter<H>` (for native-fn dispatch + global env access).
//!
//! The VM is a thin wrapper: stack of `Value`s + stack of `Frame`s +
//! IP. Native fns and closures both go through `Interpreter::apply`
//! semantics — same `FnRegistry`, same `Env`, same Value type as the
//! tree-walker. This means primitives written for the eval crate
//! (arithmetic, list, hash-map, channel, ...) just work.

use std::sync::{Arc, Mutex};

use tatara_lisp::Span;
use thiserror::Error;

use super::chunk::{CaptureSource, Chunk, CompiledFn};
use super::op::Op;
use crate::eval::Interpreter;
use crate::value::Value;

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
    /// Captured upvalues from this closure's enclosing scope. Indexed
    /// by `LoadCaptured(idx)` / `StoreCaptured(idx)`. Each cell is
    /// shared via `Mutex` so `set!` on a captured name is visible to
    /// other closures that captured the same outer slot.
    captures: Vec<Arc<Mutex<Value>>>,
    /// Heap-promoted cells for THIS frame's locals that have been
    /// captured by inner closures. Keyed by local slot index;
    /// established lazily on first `MakeClosure` that references the
    /// slot. After promotion, `set!`/`StoreLocal` writes through the
    /// cell so every closure capturing the slot sees the change.
    local_cells: std::collections::HashMap<usize, Arc<Mutex<Value>>>,
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
            captures: Vec::new(),
            local_cells: std::collections::HashMap::new(),
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
                    // If the slot has been promoted to a cell (because
                    // an inner closure captured it), read through the
                    // cell so we see any set! made via StoreCaptured.
                    if let Some(cell) = f.local_cells.get(&idx) {
                        let v = cell.lock().unwrap().clone();
                        self.stack.push(v);
                    } else {
                        let abs = f.locals_base + idx;
                        let v = self
                            .stack
                            .get(abs)
                            .cloned()
                            .ok_or(VmError::BadLocal(idx))?;
                        self.stack.push(v);
                    }
                }
                Op::StoreLocal(idx) => {
                    let v = self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                    let f = &self.frames[frame_idx];
                    // Same dual path: write through the cell when
                    // the slot has been promoted.
                    if let Some(cell) = f.local_cells.get(&idx).cloned() {
                        *cell.lock().unwrap() = v;
                    } else {
                        let abs = f.locals_base + idx;
                        if abs >= self.stack.len() {
                            return Err(VmError::BadLocal(idx));
                        }
                        self.stack[abs] = v;
                    }
                }

                Op::LoadCaptured(idx) => {
                    let f = &self.frames[frame_idx];
                    let cell = f
                        .captures
                        .get(idx)
                        .cloned()
                        .ok_or(VmError::BadLocal(idx))?;
                    let v = cell.lock().unwrap().clone();
                    self.stack.push(v);
                }
                Op::StoreCaptured(idx) => {
                    let v = self.stack.pop().ok_or(VmError::Underflow { ip: 0 })?;
                    let f = &self.frames[frame_idx];
                    let cell = f
                        .captures
                        .get(idx)
                        .cloned()
                        .ok_or(VmError::BadLocal(idx))?;
                    *cell.lock().unwrap() = v;
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
                    // Build the captures array for the new closure.
                    // For each capture descriptor: pull the cell
                    // from the appropriate slot in the CURRENT frame.
                    // Promotion: when a Local source first appears,
                    // promote the slot to a heap cell (insert into
                    // local_cells, copy the current stack value into
                    // the cell). Subsequent MakeClosures referencing
                    // the same slot reuse that same cell — so set!
                    // through any closure is observable to all
                    // closures sharing the slot.
                    let mut closure_captures: Vec<Arc<Mutex<Value>>> =
                        Vec::with_capacity(body.captures.len());
                    for (_, source) in &body.captures {
                        let cell = match source {
                            CaptureSource::Local(local_idx) => {
                                let f = &mut self.frames[frame_idx];
                                if let Some(existing) = f.local_cells.get(local_idx).cloned() {
                                    existing
                                } else {
                                    let abs = f.locals_base + local_idx;
                                    let v = self
                                        .stack
                                        .get(abs)
                                        .cloned()
                                        .ok_or(VmError::BadLocal(*local_idx))?;
                                    let cell = Arc::new(Mutex::new(v));
                                    self.frames[frame_idx]
                                        .local_cells
                                        .insert(*local_idx, cell.clone());
                                    cell
                                }
                            }
                            CaptureSource::Captured(cap_idx) => self.frames[frame_idx]
                                .captures
                                .get(*cap_idx)
                                .cloned()
                                .ok_or(VmError::BadLocal(*cap_idx))?,
                        };
                        closure_captures.push(cell);
                    }
                    let compiled = CompiledClosure {
                        body: Arc::new(body),
                        captures: closure_captures,
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
            f.captures = cc.captures.clone();
            f.local_cells.clear();
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
                captures: cc.captures.clone(),
                local_cells: std::collections::HashMap::new(),
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
/// Each closure carries a `captures` array — one cell per free
/// variable identified at compile time. The cells are
/// `Arc<Mutex<Value>>` so multiple closures sharing the same outer
/// binding (`set!` semantics) see each other's writes.
#[derive(Clone)]
pub struct CompiledClosure {
    pub body: Arc<CompiledFn>,
    pub captures: Vec<Arc<Mutex<Value>>>,
}

impl std::fmt::Debug for CompiledClosure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledClosure")
            .field("params", &self.body.params)
            .field("ops_len", &self.body.ops.len())
            .field("captures", &self.captures.len())
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

    // ── VM Phase 3: closure capture of outer locals ───────────────

    #[test]
    fn closure_captures_outer_let_local() {
        // (let ((x 10)) ((lambda (y) (+ x y)) 5)) → 15
        // Without capture-aware compilation this would fail with
        // "unbound symbol x" in the lambda body.
        let v = run_vm("(let ((x 10)) ((lambda (y) (+ x y)) 5))");
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn closure_returned_from_let_still_sees_captured() {
        // Classic make-adder pattern: closure outlives the
        // enclosing scope. Captures must be by-cell (Arc<Mutex>),
        // not by-frame-position, so the lambda still resolves x
        // after the let frame is gone.
        let v = run_vm(
            "(define (make-adder n) (lambda (x) (+ x n)))
             ((make-adder 10) 32)",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn nested_closures_chain_captures() {
        // (let ((x 5))
        //   (let ((f (lambda (a) (lambda (b) (+ x a b)))))
        //     ((f 3) 4)))
        // → 5 + 3 + 4 = 12. The inner lambda captures x via the
        // outer lambda's captures (chained), and a directly from the
        // outer lambda's locals.
        let v = run_vm(
            "(let ((x 5))
               (let ((f (lambda (a) (lambda (b) (+ x a b)))))
                 ((f 3) 4)))",
        );
        assert!(matches!(v, Value::Int(12)));
    }

    #[test]
    fn closure_set_on_captured_propagates() {
        // Two closures sharing the same outer binding. Setting x
        // through one should be visible through the other (the
        // captures are by-reference cells).
        let v = run_vm(
            "(define get (let ((x 0))
                           (define setter (lambda (v) (set! x v)))
                           (define getter (lambda () x))
                           (setter 42)
                           getter))
             (get)",
        );
        assert!(matches!(v, Value::Int(42)));
    }
}
