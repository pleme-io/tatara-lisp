//! Compiler — `Spanned` → `Chunk`.
//!
//! Single-pass post-macro-expansion walk. The eval crate's existing
//! `SpannedExpander` is run BEFORE this compiler sees the form, so
//! macros are already gone. Special forms are recognized by head
//! symbol; lambdas register a `CompiledFn` in the chunk's `fn_table`
//! and emit `MakeClosure(idx)`. Everything else lowers to `LoadGlobal
//! + Call(arity)` (or `TailCall` in tail position).

use std::sync::Arc;

use tatara_lisp::{Atom, Span, Spanned, SpannedForm};
use thiserror::Error;

use super::chunk::{Chunk, CompiledFn};
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
/// The chunk's `top` function evaluates each form in order and
/// returns the value of the last one (or `Nil` if the program is
/// empty).
pub fn compile_program(forms: &[Spanned]) -> Result<Chunk, CompileError> {
    let mut chunk = Chunk::default();
    let mut compiler = Compiler::new(&mut chunk);
    compiler.compile_top(forms)?;
    Ok(chunk)
}

/// Local scope — one frame's variables. The compiler pushes a fresh
/// `Scope` per `let` / lambda body; `LoadLocal` indices are the
/// position of the binding within the FLATTENED locals array of the
/// enclosing function.
#[derive(Debug, Default, Clone)]
struct Scope {
    /// Locally-bound names with their indices (relative to the
    /// containing function's locals_base).
    bindings: Vec<(Arc<str>, usize)>,
}

/// Per-function compiler state. Each lambda body gets a fresh one;
/// the top-level program reuses one for all top-level forms.
pub struct Compiler<'a> {
    pub(super) chunk: &'a mut Chunk,
    /// Op stream for the function currently being compiled.
    ops: Vec<Op>,
    /// Span per op (parallel to `ops`).
    spans: Vec<Span>,
    /// Stack of nested scopes (lexical lets).
    scopes: Vec<Scope>,
    /// Highest-water-mark of locals (for the function's `locals` field).
    locals_count: usize,
    /// Param count of the current function — params occupy local
    /// slots 0..params. New `let` bindings start at `params`.
    /// Reserved for diagnostics / future use; reads will land with
    /// frame-level inspection in the LSP.
    #[allow(dead_code)]
    params_count: usize,
    /// Next free local-slot index. Bumped by `alloc_local`,
    /// independent of when each allocation gets attached to a
    /// `scope_define` (so multi-binding lets allocate distinct slots
    /// even before bindings get committed to the scope).
    next_local: usize,
}

impl<'a> Compiler<'a> {
    pub fn new(chunk: &'a mut Chunk) -> Self {
        Self {
            chunk,
            ops: Vec::new(),
            spans: Vec::new(),
            scopes: vec![Scope::default()],
            locals_count: 0,
            params_count: 0,
            next_local: 0,
        }
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

        // Snapshot into the chunk's `top` field.
        self.chunk.top = CompiledFn {
            params: Vec::new(),
            rest: None,
            locals: self.locals_count,
            ops: std::mem::take(&mut self.ops),
            spans: std::mem::take(&mut self.spans),
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
                // Quasi-quote machinery isn't implemented in the VM
                // yet — fall back to a runtime-error at execution time.
                // A full implementation lifts the body to a runtime
                // value tree.
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
                if let Some(local_idx) = self.resolve_local(name) {
                    self.emit_op(Op::LoadLocal(local_idx), span);
                } else {
                    let name_idx = self.chunk.names.intern(name.as_str());
                    self.emit_op(Op::LoadGlobal(name_idx), span);
                }
            }
        }
        Ok(())
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
            // Default: function call.
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
        // Test
        self.compile_form(&items[1], false)?;
        let jmp_to_else = self.emit_placeholder(Op::JmpNot(0), items[1].span);
        // Then branch (tail position propagates from outer if).
        self.compile_form(&items[2], tail)?;
        let jmp_to_end = self.emit_placeholder(Op::Jmp(0), items[2].span);
        // Else branch.
        let else_target = self.ops.len();
        if items.len() == 4 {
            self.compile_form(&items[3], tail)?;
        } else {
            self.emit_op(Op::Nil, span);
        }
        let end_target = self.ops.len();
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
                // (define name expr) returns nil; emit Dup + StoreGlobal
                // so the defined value is on the stack before we drop it.
                let name_idx = self.chunk.names.intern(name.as_str());
                self.emit_op(Op::StoreGlobal(name_idx), span);
                self.emit_op(Op::Nil, span);
                Ok(())
            }
            SpannedForm::List(head) if !head.is_empty() => {
                // (define (name args...) body...) → (define name (lambda (args) body))
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

        // Compile each binding's value FIRST (parallel binding semantics
        // — values evaluated in outer scope), then push a new scope and
        // store each into a local.
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
        // Stores are in reverse order — top of stack is the LAST
        // binding's value.
        for (_, local_idx) in binding_locals.iter().rev() {
            self.emit_op(Op::StoreLocal(*local_idx), span);
        }
        // Push scope after all values pushed but before body, so body
        // can see them.
        self.push_scope();
        for (name, idx) in &binding_locals {
            self.scope_define(name.clone(), *idx);
        }

        // Body.
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
        // Parse params + rest.
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

        // Compile the body in a sub-compiler; it gets its own ops/spans
        // and a fresh scope. Params occupy local slots 0..params.
        let initial_locals = params.len() + usize::from(rest.is_some());
        let mut sub = Compiler {
            chunk: self.chunk,
            ops: Vec::new(),
            spans: Vec::new(),
            scopes: vec![Scope::default()],
            locals_count: initial_locals,
            params_count: initial_locals,
            next_local: initial_locals,
        };
        for (i, name) in params.iter().enumerate() {
            sub.scope_define(name.clone(), i);
        }
        if let Some(name) = &rest {
            let idx = params.len();
            sub.scope_define(name.clone(), idx);
        }
        // Compile body forms; last one is in tail position.
        let body = &items[2..];
        let last = body.len() - 1;
        for (i, form) in body.iter().enumerate() {
            sub.compile_form(form, i == last)?;
            if i != last {
                sub.emit_op(Op::Pop, form.span);
            }
        }
        sub.emit_op(Op::Return, span);

        let compiled = CompiledFn {
            params,
            rest,
            locals: sub.locals_count,
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
        // Try local first, then fall back to global.
        if let Some(local_idx) = self.resolve_local(name) {
            self.emit_op(Op::StoreLocal(local_idx), span);
        } else {
            let name_idx = self.chunk.names.intern(name);
            self.emit_op(Op::StoreGlobal(name_idx), span);
        }
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
        // Short-circuit: keep value on stack; jump to end when falsy.
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
        let end = self.ops.len();
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
        let end = self.ops.len();
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
        // Emit: (if x #f #t)
        let jmp_to_false = self.emit_placeholder(Op::JmpNot(0), span);
        self.emit_op(Op::False, span);
        let jmp_to_end = self.emit_placeholder(Op::Jmp(0), span);
        let true_target = self.ops.len();
        self.patch_jmp(jmp_to_false, true_target);
        self.emit_op(Op::True, span);
        let end = self.ops.len();
        self.patch_jmp(jmp_to_end, end);
        Ok(())
    }

    fn compile_call(
        &mut self,
        items: &[Spanned],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        // Push callable.
        self.compile_form(&items[0], false)?;
        // Push args.
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
        self.ops.push(op);
        self.spans.push(span);
    }

    fn emit_const(&mut self, v: Value, span: Span) {
        let idx = self.chunk.consts.push(v);
        self.emit_op(Op::Const(idx), span);
    }

    /// Emit a placeholder jump op. Returns the IP of the op so the
    /// caller can patch the target later. Use `patch_jmp(ip, target)`.
    fn emit_placeholder(&mut self, op: Op, span: Span) -> usize {
        let ip = self.ops.len();
        self.emit_op(op, span);
        ip
    }

    fn patch_jmp(&mut self, ip: usize, target: usize) {
        let op = match &self.ops[ip] {
            Op::Jmp(_) => Op::Jmp(target),
            Op::JmpNot(_) => Op::JmpNot(target),
            Op::JmpIf(_) => Op::JmpIf(target),
            other => panic!("patch_jmp on non-jmp op: {other:?}"),
        };
        self.ops[ip] = op;
    }

    fn alloc_local(&mut self) -> usize {
        let idx = self.next_local;
        self.next_local += 1;
        self.locals_count = self.locals_count.max(self.next_local);
        idx
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn scope_define(&mut self, name: Arc<str>, idx: usize) {
        self.scopes
            .last_mut()
            .expect("at least one scope")
            .bindings
            .push((name, idx));
    }

    fn resolve_local(&self, name: &str) -> Option<usize> {
        for scope in self.scopes.iter().rev() {
            for (n, idx) in scope.bindings.iter().rev() {
                if &**n == name {
                    return Some(*idx);
                }
            }
        }
        None
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
        // (+ 1 2) → LoadGlobal "+" / Int 1 / Int 2 / TailCall(2) / Halt
        // Top-level last form is in tail position; final TailCall feeds Halt.
        assert!(matches!(c.top.ops[0], Op::LoadGlobal(_)));
        assert!(matches!(c.top.ops[1], Op::Int(1)));
        assert!(matches!(c.top.ops[2], Op::Int(2)));
        assert!(matches!(c.top.ops[3], Op::TailCall(2)));
    }

    #[test]
    fn compile_if_emits_jumps() {
        let c = compile_str("(if #t 1 2)");
        // Test → JmpNot → Then → Jmp → Else
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
}
