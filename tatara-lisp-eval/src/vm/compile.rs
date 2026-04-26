//! Compiler — `Spanned` → `Chunk`.
//!
//! Single-pass post-macro-expansion walk. The eval crate's existing
//! `SpannedExpander` is run BEFORE this compiler sees the form, so
//! macros are already gone. Special forms are recognized by head
//! symbol; lambdas register a `CompiledFn` in the chunk's `fn_table`
//! and emit `MakeClosure(idx)`. Everything else lowers to `LoadGlobal
//! + Call(arity)` (or `TailCall` in tail position).
//!
//! Closure capture (upvalues): the compiler maintains a STACK of
//! per-function compilers. Name resolution walks the stack:
//!
//!   1. Innermost function's lexical scopes → `LoadLocal(idx)`.
//!   2. An outer function's lexical scopes → record the binding as a
//!      capture in every function between the discovery point and the
//!      innermost; emit `LoadCaptured(idx)` in the innermost. The
//!      VM populates the captures array at `MakeClosure` time by
//!      reading the parent frame's locals (or its captures, when the
//!      chain spans multiple functions).
//!   3. Not found anywhere → `LoadGlobal(name_idx)`.
//!
//! This is the canonical Lua-style upvalue scheme adapted for our
//! immutable-locals model: each captured slot is a snapshot taken
//! when the inner closure is built. `set!` on a captured name
//! writes through `StoreCaptured`, mutating the closure's own copy
//! (no shared upvalue cells in Phase 3 — we add those if the use
//! case demands it).

use std::sync::Arc;

use tatara_lisp::{Atom, Span, Spanned, SpannedForm};
use thiserror::Error;

use super::chunk::{CaptureSource, Chunk, CompiledFn};
use super::op::Op;
use crate::value::Value;

#[derive(Debug, Error)]
pub enum CompileError {
    #[error("at {at}: {message}")]
    Bad {
        at: Span,
        message: String,
    },
}

impl CompileError {
    fn bad(at: Span, message: impl Into<String>) -> Self {
        Self::Bad {
            at,
            message: message.into(),
        }
    }
}

/// Compile a sequence of top-level forms (the program) into a `Chunk`.
pub fn compile_program(forms: &[Spanned]) -> Result<Chunk, CompileError> {
    let mut chunk = Chunk::default();
    let mut compiler = Compiler::new(&mut chunk);
    compiler.compile_top(forms)?;
    Ok(chunk)
}

/// One lexical scope (a `let` body or a function's param list).
#[derive(Debug, Default, Clone)]
struct Scope {
    bindings: Vec<(Arc<str>, usize)>,
}

/// Per-function compiler state. The outer `Compiler` owns a stack of
/// these — one per nested function being compiled.
#[derive(Debug, Default)]
struct FnCompiler {
    /// Function bytecode being assembled.
    ops: Vec<Op>,
    /// Span per op (parallel to `ops`).
    spans: Vec<Span>,
    /// Stack of lexical scopes within this function.
    scopes: Vec<Scope>,
    /// Highest-water-mark for locals (param count + max simultaneous lets).
    locals_count: usize,
    /// Param count of this function. Local indices 0..params_count
    /// are params; new bindings come after.
    params_count: usize,
    /// Next free local slot — bumped by `alloc_local`.
    next_local: usize,
    /// Free variables this function captures from enclosing scopes.
    /// Each entry: (name, source). `source` says where to pull the
    /// value from in the parent frame at MakeClosure time.
    captures: Vec<(Arc<str>, CaptureSource)>,
}

impl FnCompiler {
    fn new() -> Self {
        Self {
            scopes: vec![Scope::default()],
            ..Default::default()
        }
    }

    /// Resolve a name within THIS function's scopes only. Returns
    /// the local slot index, or `None`.
    fn resolve_in_scopes(&self, name: &str) -> Option<usize> {
        for scope in self.scopes.iter().rev() {
            for (n, idx) in scope.bindings.iter().rev() {
                if &**n == name {
                    return Some(*idx);
                }
            }
        }
        None
    }

    /// Find an existing capture entry for `name`, or add a fresh one
    /// at the end. Returns the index in `captures` (which is also
    /// the index `LoadCaptured` will reference).
    fn intern_capture(&mut self, name: &Arc<str>, source: CaptureSource) -> usize {
        for (i, (n, src)) in self.captures.iter().enumerate() {
            if &**n == &**name && *src == source {
                return i;
            }
        }
        let idx = self.captures.len();
        self.captures.push((name.clone(), source));
        idx
    }
}

/// Outer compiler — owns the chunk + a stack of in-flight function
/// compilers. The "current" function is `fn_stack.last()`. Lambda
/// compilation pushes a new `FnCompiler`, compiles its body, pops it,
/// stores the result in `chunk.fn_table`, and emits MakeClosure in
/// the now-current outer function.
pub struct Compiler<'a> {
    pub(super) chunk: &'a mut Chunk,
    fn_stack: Vec<FnCompiler>,
}

impl<'a> Compiler<'a> {
    pub fn new(chunk: &'a mut Chunk) -> Self {
        Self {
            chunk,
            fn_stack: vec![FnCompiler::new()],
        }
    }

    fn current(&self) -> &FnCompiler {
        self.fn_stack.last().expect("at least one function on stack")
    }

    fn current_mut(&mut self) -> &mut FnCompiler {
        self.fn_stack.last_mut().expect("at least one function on stack")
    }

    fn compile_top(&mut self, forms: &[Spanned]) -> Result<(), CompileError> {
        if forms.is_empty() {
            self.emit_op(Op::Nil, Span::synthetic());
        } else {
            let last = forms.len() - 1;
            for (i, form) in forms.iter().enumerate() {
                self.compile_form(form, /*tail=*/ i == last)?;
                if i != last {
                    self.emit_op(Op::Pop, form.span);
                }
            }
        }
        self.emit_op(Op::Halt, Span::synthetic());

        // Snapshot the top-level fn into chunk.top.
        let top = self.fn_stack.pop().expect("top fn on stack");
        self.chunk.top = CompiledFn {
            params: Vec::new(),
            rest: None,
            locals: top.locals_count,
            captures: Vec::new(),
            ops: top.ops,
            spans: top.spans,
            source_span: Span::synthetic(),
        };
        Ok(())
    }

    /// Compile ONE form. `tail` indicates that the form's value
    /// becomes the function's return value, enabling `TailCall` for
    /// closure applications in tail position.
    fn compile_form(&mut self, form: &Spanned, tail: bool) -> Result<(), CompileError> {
        match &form.form {
            SpannedForm::Nil => self.emit_op(Op::Nil, form.span),
            SpannedForm::Atom(a) => self.compile_atom(a, form.span)?,
            SpannedForm::List(items) if items.is_empty() => {
                self.emit_op(Op::Nil, form.span);
            }
            SpannedForm::List(items) => self.compile_list(items, form.span, tail)?,
            SpannedForm::Quote(inner) => {
                let v = crate::code::spanned_to_value(inner);
                self.emit_const(v, form.span);
            }
            SpannedForm::Quasiquote(_)
            | SpannedForm::Unquote(_)
            | SpannedForm::UnquoteSplice(_) => {
                return Err(CompileError::bad(
                    form.span,
                    "quasiquote / unquote not yet supported in VM — use the tree-walker path \
                     (Interpreter::eval_program) for code that uses them",
                ));
            }
        }
        Ok(())
    }

    fn compile_atom(&mut self, a: &Atom, span: Span) -> Result<(), CompileError> {
        match a {
            Atom::Bool(true) => self.emit_op(Op::True, span),
            Atom::Bool(false) => self.emit_op(Op::False, span),
            Atom::Int(n) => self.emit_op(Op::Int(*n), span),
            Atom::Float(n) => {
                let idx = self.chunk.consts.push(Value::Float(*n));
                self.emit_op(Op::Const(idx), span);
            }
            Atom::Str(s) => {
                let idx = self.chunk.consts.push(Value::Str(Arc::from(s.as_str())));
                self.emit_op(Op::Const(idx), span);
            }
            Atom::Keyword(s) => {
                let idx = self
                    .chunk
                    .consts
                    .push(Value::Keyword(Arc::from(s.as_str())));
                self.emit_op(Op::Const(idx), span);
            }
            Atom::Symbol(name) => {
                self.emit_load_name(name, span);
            }
        }
        Ok(())
    }

    /// Resolve a name reference and emit the appropriate Load op.
    /// Walks the function stack: own scopes → captured (recording the
    /// chain in every intermediate function's `captures`) → global.
    fn emit_load_name(&mut self, name: &str, span: Span) {
        if let Some(local_idx) = self.current().resolve_in_scopes(name) {
            self.emit_op(Op::LoadLocal(local_idx), span);
            return;
        }
        // Check outer functions.
        let depth = self.fn_stack.len();
        if depth >= 2 {
            for outer_idx in (0..depth - 1).rev() {
                if let Some(parent_local) = self.fn_stack[outer_idx].resolve_in_scopes(name) {
                    // Found in fn_stack[outer_idx]. Record a capture
                    // chain in every function between outer_idx + 1
                    // and the current function (depth - 1).
                    let captured_idx = self.register_capture_chain(
                        Arc::from(name),
                        outer_idx,
                        CaptureSource::Local(parent_local),
                    );
                    self.emit_op(Op::LoadCaptured(captured_idx), span);
                    return;
                }
                // Also check whether the outer fn has THIS name as
                // one of ITS captures already — chained closures.
                let already = self.fn_stack[outer_idx]
                    .captures
                    .iter()
                    .position(|(n, _)| &**n == name);
                if let Some(parent_capture_idx) = already {
                    let captured_idx = self.register_capture_chain(
                        Arc::from(name),
                        outer_idx,
                        CaptureSource::Captured(parent_capture_idx),
                    );
                    self.emit_op(Op::LoadCaptured(captured_idx), span);
                    return;
                }
            }
        }
        // Fall through to global.
        let name_idx = self.chunk.names.intern(name);
        self.emit_op(Op::LoadGlobal(name_idx), span);
    }

    /// Mirror of `emit_load_name` for `set!` — emits a Store op.
    fn emit_store_name(&mut self, name: &str, span: Span) {
        if let Some(local_idx) = self.current().resolve_in_scopes(name) {
            self.emit_op(Op::StoreLocal(local_idx), span);
            return;
        }
        let depth = self.fn_stack.len();
        if depth >= 2 {
            for outer_idx in (0..depth - 1).rev() {
                if let Some(parent_local) = self.fn_stack[outer_idx].resolve_in_scopes(name) {
                    let captured_idx = self.register_capture_chain(
                        Arc::from(name),
                        outer_idx,
                        CaptureSource::Local(parent_local),
                    );
                    self.emit_op(Op::StoreCaptured(captured_idx), span);
                    return;
                }
                let already = self.fn_stack[outer_idx]
                    .captures
                    .iter()
                    .position(|(n, _)| &**n == name);
                if let Some(parent_capture_idx) = already {
                    let captured_idx = self.register_capture_chain(
                        Arc::from(name),
                        outer_idx,
                        CaptureSource::Captured(parent_capture_idx),
                    );
                    self.emit_op(Op::StoreCaptured(captured_idx), span);
                    return;
                }
            }
        }
        let name_idx = self.chunk.names.intern(name);
        self.emit_op(Op::StoreGlobal(name_idx), span);
    }

    /// Walk from the depth where the binding was found up to the
    /// current function, registering captures in each intermediate
    /// function. Returns the captured-index in the CURRENT (innermost)
    /// function.
    fn register_capture_chain(
        &mut self,
        name: Arc<str>,
        found_at: usize,
        first_source: CaptureSource,
    ) -> usize {
        let mut source = first_source;
        // Iterate from `found_at + 1` (the function ABOVE the one
        // where we found it) to the current function (depth - 1).
        let depth = self.fn_stack.len();
        let mut idx_in_outer = 0usize;
        for fn_idx in (found_at + 1)..depth {
            idx_in_outer = self.fn_stack[fn_idx].intern_capture(&name, source);
            // Next loop iteration sees the binding as captured in
            // fn_stack[fn_idx], so source should reference THAT
            // function's captures slot.
            source = CaptureSource::Captured(idx_in_outer);
        }
        idx_in_outer
    }

    fn compile_list(
        &mut self,
        items: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        let head = items[0].as_symbol();
        match head {
            Some("quote") => {
                if items.len() != 2 {
                    return Err(CompileError::bad(span, "(quote x): expected one arg"));
                }
                let v = crate::code::spanned_to_value(&items[1]);
                self.emit_const(v, span);
                Ok(())
            }
            Some("if") => self.compile_if(items, span, tail),
            Some("begin") => self.compile_begin(&items[1..], span, tail),
            Some("define") => self.compile_define(items, span),
            Some("let") => self.compile_let(items, span, tail),
            Some("lambda") => self.compile_lambda(items, span),
            Some("set!") => self.compile_set(items, span),
            Some("and") => self.compile_and(&items[1..], span, tail),
            Some("or") => self.compile_or(&items[1..], span, tail),
            Some("not") => self.compile_not(items, span),
            _ => self.compile_call(items, span, tail),
        }
    }

    // ── Special forms ────────────────────────────────────────────

    fn compile_if(
        &mut self,
        items: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        if items.len() < 3 || items.len() > 4 {
            return Err(CompileError::bad(
                span,
                format!("(if c t [e]): expected 2-3 args, got {}", items.len() - 1),
            ));
        }
        self.compile_form(&items[1], false)?;
        let jmp_to_else = self.emit_placeholder(Op::JmpNot(0), items[1].span);
        self.compile_form(&items[2], tail)?;
        let jmp_to_end = self.emit_placeholder(Op::Jmp(0), items[2].span);
        let else_target = self.current().ops.len();
        if items.len() == 4 {
            self.compile_form(&items[3], tail)?;
        } else {
            self.emit_op(Op::Nil, span);
        }
        let end_target = self.current().ops.len();
        self.patch_jmp(jmp_to_else, else_target);
        self.patch_jmp(jmp_to_end, end_target);
        Ok(())
    }

    fn compile_begin(
        &mut self,
        body: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        if body.is_empty() {
            self.emit_op(Op::Nil, span);
            return Ok(());
        }
        let last = body.len() - 1;
        for (i, form) in body.iter().enumerate() {
            self.compile_form(form, tail && i == last)?;
            if i != last {
                self.emit_op(Op::Pop, form.span);
            }
        }
        Ok(())
    }

    fn compile_define(&mut self, items: &[Spanned], span: Span) -> Result<(), CompileError> {
        if items.len() < 3 {
            return Err(CompileError::bad(
                span,
                "(define name expr) | (define (name args) body)",
            ));
        }
        match &items[1].form {
            SpannedForm::Atom(Atom::Symbol(name)) => {
                self.compile_form(&items[2], false)?;
                let name_idx = self.chunk.names.intern(name.as_str());
                self.emit_op(Op::StoreGlobal(name_idx), span);
                self.emit_op(Op::Nil, span);
                Ok(())
            }
            SpannedForm::List(head) if !head.is_empty() => {
                let name = head[0].as_symbol().ok_or_else(|| {
                    CompileError::bad(items[1].span, "define: first form-elem must be a symbol")
                })?;
                let mut lambda_form: Vec<Spanned> = Vec::with_capacity(items.len());
                lambda_form.push(Spanned::new(
                    span,
                    SpannedForm::Atom(Atom::Symbol("lambda".into())),
                ));
                lambda_form.push(Spanned::new(
                    items[1].span,
                    SpannedForm::List(head[1..].to_vec()),
                ));
                lambda_form.extend_from_slice(&items[2..]);
                let lambda = Spanned::new(span, SpannedForm::List(lambda_form));
                self.compile_form(&lambda, false)?;
                let name_idx = self.chunk.names.intern(name);
                self.emit_op(Op::StoreGlobal(name_idx), span);
                self.emit_op(Op::Nil, span);
                Ok(())
            }
            _ => Err(CompileError::bad(
                items[1].span,
                "define: name must be a symbol or (name args) form",
            )),
        }
    }

    fn compile_let(
        &mut self,
        items: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        if items.len() < 3 {
            return Err(CompileError::bad(
                span,
                "(let ((name val)...) body...): expected at least 1 binding pair + body",
            ));
        }
        let bindings = items[1].as_list().ok_or_else(|| {
            CompileError::bad(items[1].span, "let: bindings must be a list")
        })?;

        let mut binding_locals: Vec<(Arc<str>, usize)> = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let pair = binding.as_list().ok_or_else(|| {
                CompileError::bad(binding.span, "let: each binding must be (name val)")
            })?;
            if pair.len() != 2 {
                return Err(CompileError::bad(
                    binding.span,
                    "let: each binding must be exactly (name val)",
                ));
            }
            let name = pair[0].as_symbol().ok_or_else(|| {
                CompileError::bad(pair[0].span, "let: binding name must be a symbol")
            })?;
            self.compile_form(&pair[1], false)?;
            let local_idx = self.alloc_local();
            binding_locals.push((Arc::<str>::from(name), local_idx));
        }
        for (_, local_idx) in binding_locals.iter().rev() {
            self.emit_op(Op::StoreLocal(*local_idx), span);
        }
        self.push_scope();
        for (name, idx) in &binding_locals {
            self.scope_define(name.clone(), *idx);
        }

        let body = &items[2..];
        let last = body.len().saturating_sub(1);
        for (i, form) in body.iter().enumerate() {
            self.compile_form(form, tail && i == last)?;
            if i != last {
                self.emit_op(Op::Pop, form.span);
            }
        }
        if body.is_empty() {
            self.emit_op(Op::Nil, span);
        }
        self.pop_scope();
        Ok(())
    }

    fn compile_lambda(&mut self, items: &[Spanned], span: Span) -> Result<(), CompileError> {
        if items.len() < 3 {
            return Err(CompileError::bad(
                span,
                "(lambda (params) body...): expected param list + body",
            ));
        }
        let param_list: Vec<Spanned> = match &items[1].form {
            SpannedForm::Nil => Vec::new(),
            SpannedForm::List(xs) => xs.clone(),
            _ => {
                return Err(CompileError::bad(
                    items[1].span,
                    "lambda: params must be a list",
                ));
            }
        };
        let mut params: Vec<Arc<str>> = Vec::new();
        let mut rest: Option<Arc<str>> = None;
        let mut i = 0;
        while i < param_list.len() {
            let s = param_list[i].as_symbol().ok_or_else(|| {
                CompileError::bad(param_list[i].span, "lambda: param must be a symbol")
            })?;
            if s == "&rest" {
                let name = param_list
                    .get(i + 1)
                    .and_then(Spanned::as_symbol)
                    .ok_or_else(|| {
                        CompileError::bad(items[1].span, "lambda: &rest needs a name")
                    })?;
                rest = Some(Arc::<str>::from(name));
                if i + 2 != param_list.len() {
                    return Err(CompileError::bad(
                        items[1].span,
                        "lambda: &rest must be the last param",
                    ));
                }
                break;
            }
            params.push(Arc::<str>::from(s));
            i += 1;
        }

        // Push a fresh FnCompiler for the lambda body. Any captures
        // it discovers will record into THIS new FnCompiler — and
        // also into intermediate FnCompilers if the chain is nested.
        let initial_locals = params.len() + usize::from(rest.is_some());
        let mut sub = FnCompiler::new();
        sub.locals_count = initial_locals;
        sub.params_count = initial_locals;
        sub.next_local = initial_locals;
        for (i, name) in params.iter().enumerate() {
            sub.scopes[0].bindings.push((name.clone(), i));
        }
        if let Some(name) = &rest {
            sub.scopes[0]
                .bindings
                .push((name.clone(), params.len()));
        }
        self.fn_stack.push(sub);

        // Compile body forms.
        let body_forms = &items[2..];
        let last = body_forms.len() - 1;
        for (i, form) in body_forms.iter().enumerate() {
            self.compile_form(form, /*tail=*/ i == last)?;
            if i != last {
                self.emit_op(Op::Pop, form.span);
            }
        }
        self.emit_op(Op::Return, span);

        // Pop the fn compiler and store as a CompiledFn in fn_table.
        let sub = self.fn_stack.pop().expect("just pushed");
        let compiled = CompiledFn {
            params,
            rest,
            locals: sub.locals_count,
            captures: sub.captures,
            ops: sub.ops,
            spans: sub.spans,
            source_span: span,
        };
        let fn_idx = self.chunk.fn_table.len();
        self.chunk.fn_table.push(compiled);
        self.emit_op(Op::MakeClosure(fn_idx), span);
        Ok(())
    }

    fn compile_set(&mut self, items: &[Spanned], span: Span) -> Result<(), CompileError> {
        if items.len() != 3 {
            return Err(CompileError::bad(span, "(set! name expr): expected 2 args"));
        }
        let name = items[1].as_symbol().ok_or_else(|| {
            CompileError::bad(items[1].span, "set!: first arg must be a symbol")
        })?;
        self.compile_form(&items[2], false)?;
        self.emit_store_name(name, span);
        self.emit_op(Op::Nil, span);
        Ok(())
    }

    fn compile_and(
        &mut self,
        exprs: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        if exprs.is_empty() {
            self.emit_op(Op::True, span);
            return Ok(());
        }
        let mut end_jumps: Vec<usize> = Vec::new();
        let last = exprs.len() - 1;
        for (i, e) in exprs.iter().enumerate() {
            self.compile_form(e, tail && i == last)?;
            if i != last {
                self.emit_op(Op::Dup, e.span);
                let j = self.emit_placeholder(Op::JmpNot(0), e.span);
                end_jumps.push(j);
                self.emit_op(Op::Pop, e.span);
            }
        }
        let end = self.current().ops.len();
        for j in end_jumps {
            self.patch_jmp(j, end);
        }
        Ok(())
    }

    fn compile_or(
        &mut self,
        exprs: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        if exprs.is_empty() {
            self.emit_op(Op::False, span);
            return Ok(());
        }
        let mut end_jumps: Vec<usize> = Vec::new();
        let last = exprs.len() - 1;
        for (i, e) in exprs.iter().enumerate() {
            self.compile_form(e, tail && i == last)?;
            if i != last {
                self.emit_op(Op::Dup, e.span);
                let j = self.emit_placeholder(Op::JmpIf(0), e.span);
                end_jumps.push(j);
                self.emit_op(Op::Pop, e.span);
            }
        }
        let end = self.current().ops.len();
        for j in end_jumps {
            self.patch_jmp(j, end);
        }
        Ok(())
    }

    fn compile_not(&mut self, items: &[Spanned], span: Span) -> Result<(), CompileError> {
        if items.len() != 2 {
            return Err(CompileError::bad(span, "(not x): expected 1 arg"));
        }
        self.compile_form(&items[1], false)?;
        let jmp_to_false = self.emit_placeholder(Op::JmpNot(0), span);
        self.emit_op(Op::False, span);
        let jmp_to_end = self.emit_placeholder(Op::Jmp(0), span);
        let true_target = self.current().ops.len();
        self.patch_jmp(jmp_to_false, true_target);
        self.emit_op(Op::True, span);
        let end = self.current().ops.len();
        self.patch_jmp(jmp_to_end, end);
        Ok(())
    }

    fn compile_call(
        &mut self,
        items: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        self.compile_form(&items[0], false)?;
        for arg in &items[1..] {
            self.compile_form(arg, false)?;
        }
        let arity = items.len() - 1;
        if tail {
            self.emit_op(Op::TailCall(arity), span);
        } else {
            self.emit_op(Op::Call(arity), span);
        }
        Ok(())
    }

    // ── Helpers ──────────────────────────────────────────────────

    fn emit_op(&mut self, op: Op, span: Span) {
        let f = self.current_mut();
        f.ops.push(op);
        f.spans.push(span);
    }

    fn emit_const(&mut self, v: Value, span: Span) {
        let idx = self.chunk.consts.push(v);
        self.emit_op(Op::Const(idx), span);
    }

    fn emit_placeholder(&mut self, op: Op, span: Span) -> usize {
        let ip = self.current().ops.len();
        self.emit_op(op, span);
        ip
    }

    fn patch_jmp(&mut self, ip: usize, target: usize) {
        let f = self.current_mut();
        let op = match &f.ops[ip] {
            Op::Jmp(_) => Op::Jmp(target),
            Op::JmpNot(_) => Op::JmpNot(target),
            Op::JmpIf(_) => Op::JmpIf(target),
            other => panic!("patch_jmp on non-jmp op: {other:?}"),
        };
        f.ops[ip] = op;
    }

    fn alloc_local(&mut self) -> usize {
        let f = self.current_mut();
        let idx = f.next_local;
        f.next_local += 1;
        f.locals_count = f.locals_count.max(f.next_local);
        idx
    }

    fn push_scope(&mut self) {
        self.current_mut().scopes.push(Scope::default());
    }

    fn pop_scope(&mut self) {
        self.current_mut().scopes.pop();
    }

    fn scope_define(&mut self, name: Arc<str>, idx: usize) {
        self.current_mut()
            .scopes
            .last_mut()
            .expect("at least one scope")
            .bindings
            .push((name, idx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tatara_lisp::read_spanned;

    fn compile_str(src: &str) -> Chunk {
        let forms = read_spanned(src).unwrap();
        compile_program(&forms).unwrap()
    }

    #[test]
    fn compile_int_literal() {
        let c = compile_str("42");
        assert!(matches!(c.top.ops[0], Op::Int(42)));
        assert!(matches!(c.top.ops[1], Op::Halt));
    }

    #[test]
    fn compile_arithmetic_call() {
        let c = compile_str("(+ 1 2)");
        assert!(matches!(c.top.ops[0], Op::LoadGlobal(_)));
        assert!(matches!(c.top.ops[1], Op::Int(1)));
        assert!(matches!(c.top.ops[2], Op::Int(2)));
        assert!(matches!(c.top.ops[3], Op::TailCall(2)));
    }

    #[test]
    fn compile_if_emits_jumps() {
        let c = compile_str("(if #t 1 2)");
        let has_jmp_not = c.top.ops.iter().any(|o| matches!(o, Op::JmpNot(_)));
        let has_jmp = c.top.ops.iter().any(|o| matches!(o, Op::Jmp(_)));
        assert!(has_jmp_not && has_jmp);
    }

    #[test]
    fn compile_let_uses_locals() {
        let c = compile_str("(let ((x 10) (y 20)) (+ x y))");
        let has_store_local = c.top.ops.iter().any(|o| matches!(o, Op::StoreLocal(_)));
        let has_load_local = c.top.ops.iter().any(|o| matches!(o, Op::LoadLocal(_)));
        assert!(has_store_local && has_load_local);
    }

    #[test]
    fn compile_define_lambda_registers_fn() {
        let c = compile_str("(define (sq x) (* x x))");
        assert_eq!(c.fn_table.len(), 1);
        assert_eq!(c.fn_table[0].params.len(), 1);
    }

    #[test]
    fn compile_lambda_emits_make_closure() {
        let c = compile_str("(lambda (x) x)");
        let has_make = c.top.ops.iter().any(|o| matches!(o, Op::MakeClosure(0)));
        assert!(has_make);
    }

    #[test]
    fn compile_quote_yields_const() {
        let c = compile_str("'foo");
        assert!(matches!(c.top.ops[0], Op::Const(_)));
    }

    #[test]
    fn compile_lambda_captures_outer_let_local() {
        // (let ((x 10)) (lambda (y) (+ x y))) — the lambda's body
        // emits LoadCaptured for x (outer-let local) and LoadLocal
        // for y (own param). The lambda's CompiledFn must record
        // x in `captures` with source = Local(parent's x slot).
        let c = compile_str("(let ((x 10)) (lambda (y) (+ x y)))");
        assert_eq!(c.fn_table.len(), 1);
        let fn_def = &c.fn_table[0];
        assert_eq!(fn_def.captures.len(), 1);
        assert_eq!(&*fn_def.captures[0].0, "x");
        assert!(matches!(
            fn_def.captures[0].1,
            CaptureSource::Local(_)
        ));
        let has_load_captured = fn_def.ops.iter().any(|o| matches!(o, Op::LoadCaptured(_)));
        assert!(has_load_captured);
    }

    #[test]
    fn compile_nested_lambdas_chain_captures() {
        // (let ((x 5))
        //   (lambda (a) (lambda (b) (+ x a b))))
        // Inner lambda captures x (chained — from the outer lambda's
        // captures, not directly from the let) AND a (from the outer
        // lambda's locals). Outer lambda captures x from the let.
        // fn_table order is registration-order: inner is compiled
        // first (during outer's body compilation) and gets index 0;
        // outer gets index 1.
        let c = compile_str("(let ((x 5)) (lambda (a) (lambda (b) (+ x a b))))");
        assert_eq!(c.fn_table.len(), 2);
        // Inner lambda — captures both x and a.
        let inner = &c.fn_table[0];
        let names: Vec<&str> = inner.captures.iter().map(|(n, _)| &**n).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"a"));
        // x in inner should be Captured(_) — chained via outer's captures.
        let x_src = inner.captures.iter().find(|(n, _)| &**n == "x").unwrap().1;
        assert!(matches!(x_src, CaptureSource::Captured(_)));
        // a in inner should be Local(_) — outer's a is its own param.
        let a_src = inner.captures.iter().find(|(n, _)| &**n == "a").unwrap().1;
        assert!(matches!(a_src, CaptureSource::Local(_)));
        // Outer lambda — captures only x (a is its own param).
        let outer = &c.fn_table[1];
        assert_eq!(outer.captures.len(), 1);
        assert_eq!(&*outer.captures[0].0, "x");
        assert!(matches!(outer.captures[0].1, CaptureSource::Local(_)));
    }
}
