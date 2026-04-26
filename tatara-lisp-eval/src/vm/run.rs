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

/// One installed exception handler. Pushed by `PushHandler`, popped
/// by `PopHandler` or activated when an error unwinds through this
/// frame.
#[derive(Debug, Clone, Copy)]
struct HandlerRecord {
    /// IP to jump to on error.
    catch_ip: usize,
    /// Local slot to store the error Value into before resuming.
    error_local: usize,
    /// Stack length AT push time — on error, the runtime truncates
    /// any stale temporaries left over from a partially-evaluated
    /// body so handler logic starts on a clean slate.
    stack_at_push: usize,
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
    /// Active error handlers on this frame, innermost last.
    handlers: Vec<HandlerRecord>,
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
    /// final value (the program's result). Errors that escape an
    /// active `(try ...)` handler propagate to the caller.
    ///
    /// Convenience wrapper for callers holding a `&Chunk` — clones into
    /// an `Arc<Chunk>`. Prefer `run_arc` when the chunk is already
    /// `Arc`-shared (e.g. from the host interpreter's compile cache).
    pub fn run<H: 'static>(
        &mut self,
        chunk: &Chunk,
        interp: &mut Interpreter<H>,
        host: &mut H,
    ) -> Result<Value, VmError> {
        let chunk_arc = Arc::new(chunk.clone());
        self.run_arc(chunk_arc, interp, host)
    }

    /// Like `run`, but takes ownership of an `Arc<Chunk>` so closures
    /// produced via `MakeClosure` can carry a cheap clone of the chunk
    /// for self-contained re-invocation through `Caller::apply_value`.
    pub fn run_arc<H: 'static>(
        &mut self,
        chunk: Arc<Chunk>,
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
            handlers: Vec::new(),
        };
        // Reserve slots for locals.
        for _ in 0..top_locals {
            self.stack.push(Value::Nil);
        }
        self.frames.push(top_frame);

        // Drive the run loop with handler-aware error routing.
        self.run_with_handlers(&chunk, interp, host)
    }

    /// Inner main interpret loop — runs until Halt, Return at top,
    /// or an error. Errors are caught by `run_with_handlers` which
    /// routes them through any installed `(try ...)` handlers.
    /// `chunk` is `&Arc<Chunk>` (not `&Chunk`) so `Op::MakeClosure` can
    /// stash a cheap `Arc::clone` into the produced `CompiledClosure`.
    fn run_inner<H: 'static>(
        &mut self,
        chunk: &Arc<Chunk>,
        interp: &mut Interpreter<H>,
        host: &mut H,
    ) -> Result<Value, VmError> {
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
                        chunk: Arc::clone(chunk),
                        globals: interp.globals_snapshot().clone(),
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

                Op::EvalSexp(idx) => {
                    // Tree-walker fallback. The const pool entry is
                    // a `Value::Sexp(Sexp, Span)` (set up by the
                    // compiler's emit_eval_sexp). Lift back to a
                    // Spanned and call into the host interpreter.
                    let v = chunk.consts.get(idx).clone();
                    let (sexp, sp) = match v {
                        Value::Sexp(s, sp) => (s, sp),
                        _ => {
                            return Err(VmError::Eval(crate::error::EvalError::native_fn(
                                Arc::<str>::from("vm:eval-sexp"),
                                "expected a Sexp constant in EvalSexp",
                                span,
                            )));
                        }
                    };
                    let spanned = tatara_lisp::Spanned::from_sexp_at(&sexp, sp);
                    let result = interp.eval_spanned(&spanned, host)?;
                    self.stack.push(result);
                }

                Op::PushHandler {
                    catch_ip,
                    error_local,
                } => {
                    let stack_at_push = self.stack.len();
                    self.frames[frame_idx].handlers.push(HandlerRecord {
                        catch_ip,
                        error_local,
                        stack_at_push,
                    });
                }
                Op::PopHandler => {
                    self.frames[frame_idx].handlers.pop();
                }
            }
        }
    }

    /// Wrap the raw run loop so any propagating error gets routed
    /// through the nearest installed handler. Frames above the
    /// handler are unwound; the handler's frame jumps to its
    /// `catch_ip` with the error value stored at `error_local`.
    fn run_with_handlers<H: 'static>(
        &mut self,
        chunk: &Arc<Chunk>,
        interp: &mut Interpreter<H>,
        host: &mut H,
    ) -> Result<Value, VmError> {
        loop {
            match self.run_inner(chunk, interp, host) {
                Ok(v) => return Ok(v),
                Err(VmError::Eval(eval_err)) => {
                    let err_value = vm_err_to_value(&eval_err);
                    if !self.unwind_to_handler(err_value) {
                        return Err(VmError::Eval(eval_err));
                    }
                }
                Err(other) => {
                    let err_value = vm_runtime_err_to_value(&other);
                    if !self.unwind_to_handler(err_value) {
                        return Err(other);
                    }
                }
            }
        }
    }

    /// Find the nearest frame with at least one installed handler
    /// and jump to it. Pops every frame above; truncates the value
    /// stack to the handler's snapshot; stores `err_value` into the
    /// handler's `error_local`; sets the handler-frame's IP to
    /// `catch_ip`. Returns `false` if no handler is installed
    /// anywhere — caller propagates the error to the embedder.
    fn unwind_to_handler(&mut self, err_value: Value) -> bool {
        // Walk frames innermost → outermost looking for a handler.
        for frame_idx in (0..self.frames.len()).rev() {
            if !self.frames[frame_idx].handlers.is_empty() {
                // Pop every frame above this one.
                while self.frames.len() > frame_idx + 1 {
                    let f = self.frames.pop().unwrap();
                    self.stack.truncate(f.locals_base);
                }
                // Pop the most recent handler from THIS frame.
                let handler = self.frames[frame_idx]
                    .handlers
                    .pop()
                    .expect("handler present");
                // Truncate the value stack to whatever it was at
                // PushHandler time so handler logic starts clean.
                self.stack.truncate(handler.stack_at_push);
                // Store the error value into the handler's local slot.
                let abs = self.frames[frame_idx].locals_base + handler.error_local;
                // The slot may have been promoted to a cell.
                if let Some(cell) = self.frames[frame_idx]
                    .local_cells
                    .get(&handler.error_local)
                    .cloned()
                {
                    *cell.lock().unwrap() = err_value;
                } else if abs < self.stack.len() {
                    self.stack[abs] = err_value;
                } else {
                    // Grow the stack with nils until we can write.
                    while self.stack.len() <= abs {
                        self.stack.push(Value::Nil);
                    }
                    self.stack[abs] = err_value;
                }
                // Resume at the handler.
                self.frames[frame_idx].ip = handler.catch_ip;
                return true;
            }
        }
        false
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
            f.handlers.clear();
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
                handlers: Vec::new(),
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

/// Convert an `EvalError` into a `Value::Error` so a try/catch
/// handler can observe Rust-side errors uniformly with user-thrown
/// ones. User-thrown errors (carried in `EvalError::User`) preserve
/// their original `Value` for transparency.
fn vm_err_to_value(err: &crate::error::EvalError) -> Value {
    use crate::error::EvalError::*;
    if let User { value, .. } = err {
        return value.clone();
    }
    let tag: Arc<str> = match err {
        UnboundSymbol { .. } => Arc::from("unbound-symbol"),
        ArityMismatch { .. } => Arc::from("arity-mismatch"),
        TypeMismatch { .. } => Arc::from("type-mismatch"),
        DivisionByZero { .. } => Arc::from("division-by-zero"),
        NotCallable { .. } => Arc::from("not-callable"),
        BadSpecialForm { .. } => Arc::from("bad-special-form"),
        NativeFn { .. } => Arc::from("native-fn"),
        Reader(_) => Arc::from("reader"),
        Halted => Arc::from("halted"),
        NotImplemented(_) => Arc::from("not-implemented"),
        User { .. } => unreachable!(),
    };
    Value::Error(Arc::new(crate::value::ErrorObj {
        tag,
        message: Arc::from(err.short_message()),
        data: Vec::new(),
    }))
}

/// Convert a non-EvalError VM error (Underflow, BadLocal, Unbound,
/// NotCallable, Arity) into a `Value::Error` for handler routing.
fn vm_runtime_err_to_value(err: &VmError) -> Value {
    let (tag, message): (&str, String) = match err {
        VmError::Underflow { ip } => ("vm-underflow", format!("stack underflow at op {ip}")),
        VmError::Unbound { name, .. } => ("unbound-symbol", format!("unbound symbol `{name}`")),
        VmError::NotCallable { kind, .. } => {
            ("not-callable", format!("value of type {kind} is not callable"))
        }
        VmError::Arity { expected, got, .. } => (
            "arity-mismatch",
            format!("expected {expected} args, got {got}"),
        ),
        VmError::BadLocal(idx) => ("bad-local", format!("local index out of bounds: {idx}")),
        VmError::Eval(inner) => return vm_err_to_value(inner),
    };
    Value::Error(Arc::new(crate::value::ErrorObj {
        tag: Arc::from(tag),
        message: Arc::from(message),
        data: Vec::new(),
    }))
}

/// Foreign-tagged compiled closure — the VM's native callable shape.
/// Wrapping in `Foreign` lets us pass it through `Value` (which is
/// shared with the tree-walker) without growing the `Value` enum.
///
/// A `CompiledClosure` is **self-contained**: it carries everything
/// needed to re-invoke the body in isolation:
///   - the compiled body (`body`);
///   - one upvalue cell per free variable (`captures`);
///   - the enclosing chunk (`chunk`) so opcode operands referencing
///     `consts` / `names` / `fn_table` resolve correctly;
///   - a snapshot of the host's globals env at MakeClosure time
///     (`globals`). `Env` is cheap to clone — frames are shared via
///     `Arc<Mutex<...>>`, so subsequent global definitions on the
///     host interpreter are still visible through this snapshot.
///
/// The self-contained shape is what lets a `Value::Foreign(CompiledClosure)`
/// flow into a native higher-order primitive (`map`, `filter`, ...) and
/// be invoked through `Caller::apply_value` — the apply path can spin
/// up a fresh `Vm` against just the closure + a `&mut H` host without
/// needing a re-entrant `&mut Interpreter` borrow.
#[derive(Clone)]
pub struct CompiledClosure {
    pub body: Arc<CompiledFn>,
    pub captures: Vec<Arc<Mutex<Value>>>,
    pub chunk: Arc<super::chunk::Chunk>,
    pub globals: crate::env::Env,
}

impl CompiledClosure {
    /// Lift this VM-compiled closure to a tree-walker-shaped
    /// `crate::value::Closure`. Used when a native higher-order
    /// primitive (`map`, `filter`, `foldl`, ...) holds a
    /// `Value::Foreign(CompiledClosure)` and needs to invoke it
    /// through the standard `Caller::apply_value` path — that path
    /// goes through `eval::apply()` which knows how to dispatch
    /// `Value::Closure`.
    ///
    /// The lifted closure carries:
    ///   - the original Spanned body (preserved by the compiler in
    ///     `body.source_body`);
    ///   - a `captured_env` synthesized from the closure's positional
    ///     captures + the host globals snapshot.
    ///
    /// Trade-off: the lifted invocation runs through the tree-walker,
    /// not the VM. Faster paths (direct VM dispatch) are possible but
    /// would require threading mutable Interpreter state through
    /// `Caller`. Correctness-wise the tree-walker is authoritative —
    /// the VM is parity-validated against it.
    ///
    /// Mutation note: `set!` performed inside the lifted closure
    /// writes to the lifted `captured_env`, NOT to the original
    /// upvalue cells. For HoF callbacks this is the common case
    /// (read-only captures); closures that need shared `set!`
    /// semantics should be invoked through the VM directly.
    pub fn lift_to_closure(&self) -> Arc<crate::value::Closure> {
        let mut captured_env = self.globals.clone();
        captured_env.push();
        for ((name, _), cell) in self.body.captures.iter().zip(self.captures.iter()) {
            let v = cell.lock().unwrap().clone();
            captured_env.define(name.clone(), v);
        }
        Arc::new(crate::value::Closure {
            params: self.body.params.clone(),
            rest: self.body.rest.clone(),
            body: self.body.source_body.clone(),
            captured_env,
            source: self.body.source_span,
        })
    }
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

    // ── VM Phase 4: try / catch ────────────────────────────────────

    #[test]
    fn try_returns_body_value_when_no_throw() {
        let v = run_vm(
            "(try
               (+ 1 2 3)
               (catch (e) :unreachable))",
        );
        assert!(matches!(v, Value::Int(6)));
    }

    #[test]
    fn try_catches_user_throw() {
        let v = run_vm(
            "(try
               (throw (ex-info \"boom\" (list)))
               (catch (e) (error-message e)))",
        );
        match v {
            Value::Str(s) => assert_eq!(&*s, "boom"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn try_catches_runtime_error() {
        // Type mismatch from a primitive — Rust-side error, not user
        // throw. The VM converts it to a Value::Error and routes
        // through the handler.
        let v = run_vm(
            "(try
               (+ 1 \"oops\")
               (catch (e) (error-tag e)))",
        );
        // type-mismatch is the canonical tag for type errors.
        match v {
            Value::Keyword(s) => assert_eq!(&*s, "type-mismatch"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn try_inside_a_function_body() {
        // The try frame is the lambda's frame, not the top-level.
        // Verifies handler unwinding when the inner frame is the
        // one with the handler installed.
        let v = run_vm(
            "(define (safe-div a b)
               (try
                 (/ a b)
                 (catch (e) :div-failed)))
             (safe-div 10 0)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "div-failed"));
    }

    #[test]
    fn nested_try_inner_catches_first() {
        let v = run_vm(
            "(try
               (try
                 (throw (ex-info \"inner\" (list)))
                 (catch (e) :inner-caught))
               (catch (e) :outer-caught))",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "inner-caught"));
    }

    // ── VM Phase 5: tree-walker fallback ───────────────────────────

    #[test]
    fn vm_falls_back_to_tree_walker_for_quasi_quote() {
        // Quasi-quote isn't a VM opcode; the compiler emits EvalSexp,
        // which dispatches the form through Interpreter::eval_spanned
        // at runtime. The dispatch only sees globals (not VM locals)
        // — that's an acknowledged limitation of EvalSexp fallback.
        // Use a (define) so x is a global the tree-walker can see.
        let v = run_vm("(define x 99) `(a ,x c)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[1], Value::Int(99)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn vm_falls_back_for_eval() {
        // (eval '(+ 1 2 3)) — runtime metaprogramming, falls back.
        let v = run_vm("(eval '(+ 1 2 3))");
        assert!(matches!(v, Value::Int(6)));
    }

    #[test]
    fn vm_falls_back_for_macroexpand() {
        // The macroexpand introspection special form is in the
        // fallback list; the VM defers to the tree-walker.
        let v = run_vm(
            "(defmacro twice (x) `(* ,x 2))
             (macroexpand-1 '(twice 7))",
        );
        // (twice 7) → (* 7 2) — list-shape with three symbol/int
        // elements.
        match v {
            Value::List(xs) => assert_eq!(xs.len(), 3),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn try_handler_can_rethrow_to_outer() {
        let v = run_vm(
            "(try
               (try
                 (throw (ex-info \"first\" (list)))
                 (catch (e) (throw (ex-info \"rethrown\" (list)))))
               (catch (e) (error-message e)))",
        );
        match v {
            Value::Str(s) => assert_eq!(&*s, "rethrown"),
            other => panic!("{other:?}"),
        }
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
