//! Core evaluator.
//!
//! Threads a mutable `Env` and the immutable `FnRegistry<H>` through
//! recursive eval. Special forms are dispatched by head symbol before
//! function application. Closures capture a snapshot of the current env
//! at lambda creation; native functions live in the registry and are
//! referred to in values by name.

use std::sync::Arc;

use tatara_lisp::{Atom, MacroDef, Param, Span, Spanned, SpannedExpander, SpannedForm};

use crate::code::{spanned_to_value, value_to_spanned};
use crate::env::Env;
use crate::error::{EvalError, Result};
use crate::ffi::{
    Arity, Caller, FnEntry, FnImpl, FnRegistry, FromValue, HigherOrderCallable, IntoValue,
    NativeCallable,
};
use crate::module::{Loader, Module, ModuleError, ModuleRegistry, NoLoader};
use crate::special::SpecialForm;
use crate::value::{Closure, ErrorObj, NativeFn, Value};

/// An embedded tatara-lisp evaluator, parameterized over the host context
/// `H` that registered functions read/write.
pub struct Interpreter<H> {
    pub(crate) registry: FnRegistry<H>,
    pub(crate) globals: Env,
    /// Span-preserving macro expander. Top-level `defmacro`,
    /// `defpoint-template`, and `defcheck` forms register here; macro calls
    /// in subsequent forms are rewritten before evaluation. Persisted across
    /// `eval_program` calls so REPL sessions accumulate macros naturally.
    pub(crate) expander: SpannedExpander,
    /// Module table — populated as `(require ...)` loads files. Shared
    /// across all `Interpreter`s that share a registry (cloning an
    /// `Interpreter` for sub-eval reuses the same registry).
    pub(crate) modules: ModuleRegistry,
    /// Source loader for `(require ...)`. Embedders inject filesystem
    /// access here; the default `NoLoader` rejects every require.
    pub(crate) loader: Arc<dyn Loader>,
    /// Path of the module currently being evaluated. `(provide ...)`
    /// adds names to whichever module owns this path. Top-level eval
    /// (not inside any `(require)`) uses an empty path which means
    /// "no current module" — `provide` errors there.
    pub(crate) current_module: Option<Arc<str>>,
}

impl<H: 'static> Interpreter<H> {
    pub fn new() -> Self {
        Self {
            registry: FnRegistry::new(),
            globals: Env::new(),
            expander: SpannedExpander::new(),
            modules: ModuleRegistry::new(),
            loader: Arc::new(NoLoader),
            current_module: None,
        }
    }

    /// Replace the source loader. Required for `(require ...)` to do
    /// anything useful — the default `NoLoader` rejects every require.
    pub fn set_loader(&mut self, loader: Arc<dyn Loader>) {
        self.loader = loader;
    }

    /// Borrow the module registry. Useful for tests + inspection.
    pub fn modules(&self) -> &ModuleRegistry {
        &self.modules
    }

    /// Register a native Rust function, exposing it to Lisp code under
    /// `name`. Re-registering the same name overwrites the prior entry
    /// (last-write-wins) and leaves the global binding intact.
    pub fn register_fn<F>(&mut self, name: impl Into<Arc<str>>, arity: Arity, callable: F)
    where
        F: NativeCallable<H>,
    {
        let name = name.into();
        self.registry.insert(FnEntry {
            name: name.clone(),
            arity,
            callable: FnImpl::Native(Arc::new(callable)),
        });
        self.globals.define(
            name.clone(),
            Value::NativeFn(Arc::new(NativeFn { name, arity })),
        );
    }

    /// Register a higher-order Rust primitive — receives a `Caller` so it
    /// can invoke `Value::Closure` / `Value::NativeFn` arguments back into
    /// the eval loop. Used for `map`, `filter`, `fold`, `apply`,
    /// `for-each`, etc. Same overwrite semantics as `register_fn`.
    pub fn register_higher_order_fn<F>(
        &mut self,
        name: impl Into<Arc<str>>,
        arity: Arity,
        callable: F,
    ) where
        F: HigherOrderCallable<H>,
    {
        let name = name.into();
        self.registry.insert(FnEntry {
            name: name.clone(),
            arity,
            callable: FnImpl::Higher(Arc::new(callable)),
        });
        self.globals.define(
            name.clone(),
            Value::NativeFn(Arc::new(NativeFn { name, arity })),
        );
    }

    /// Evaluate a single already-read spanned form in this interpreter's
    /// global environment. Macro expansion runs first if any macros are
    /// registered. Bare `eval_spanned` does NOT register top-level
    /// `defmacro` — `eval_top_form` is the entry point for that.
    pub fn eval_spanned(&mut self, form: &Spanned, host: &mut H) -> Result<Value> {
        let expanded = self.fully_expand(form, host)?;
        eval_in(
            &mut self.globals,
            &self.registry,
            &self.expander,
            &expanded,
            host,
        )
    }

    /// Evaluate a slice of forms in order, returning the last result.
    ///
    /// Top-level `defmacro` / `defpoint-template` / `defcheck` forms register
    /// into the persistent expander and yield `Value::Nil`. All other forms
    /// are fully expanded (recursively rewriting macro calls anywhere
    /// in the form tree, with each macro body run through the live
    /// evaluator at expansion time) before being evaluated. This is the
    /// canonical entry point for running a tatara-lisp program — REPL,
    /// embedded host, batch script.
    ///
    /// Empty input returns `Value::Nil`.
    pub fn eval_program(&mut self, forms: &[Spanned], host: &mut H) -> Result<Value> {
        let mut last = Value::Nil;
        for form in forms {
            last = self.eval_top_form(form, host)?;
        }
        Ok(last)
    }

    /// Evaluate one top-level form: register macros, handle module-
    /// system forms (`provide` / `require`), expand, then eval.
    /// Public so embedders that drive the read-eval loop themselves
    /// (REPL, hot-reload watchers) can preserve top-level semantics
    /// without re-implementing the registration handshake.
    pub fn eval_top_form(&mut self, form: &Spanned, host: &mut H) -> Result<Value> {
        if self.expander.try_register_macro(form)? {
            return Ok(Value::Nil);
        }
        // Handle module-system forms BEFORE general expansion. They
        // need `&mut self` access (loader, module registry, current
        // module) which the generic eval dispatch can't carry.
        if let Some(head) = head_symbol(form) {
            match head {
                "provide" => return self.eval_provide(form, host),
                "require" => return self.eval_require(form, host),
                _ => {}
            }
        }
        let expanded = self.fully_expand(form, host)?;
        eval_in(
            &mut self.globals,
            &self.registry,
            &self.expander,
            &expanded,
            host,
        )
    }

    /// Top-level `(provide name1 name2 ...)`. Adds each name to the
    /// current module's export set. Errors if not currently inside a
    /// module load (i.e. running at the embedder's top level).
    fn eval_provide(&mut self, form: &Spanned, _host: &mut H) -> Result<Value> {
        let items = form.as_list().unwrap_or(&[]);
        let span = form.span;
        let Some(current) = self.current_module.clone() else {
            return Err(EvalError::bad_form(
                "provide",
                "`provide` only valid at module top level — embedder evaluating top-level code has no current module",
                span,
            ));
        };
        // Collect names to export.
        let mut names: Vec<Arc<str>> = Vec::with_capacity(items.len().saturating_sub(1));
        for item in &items[1..] {
            let name = item.as_symbol().ok_or_else(|| {
                EvalError::bad_form(
                    "provide",
                    "expected symbol — every arg must name a binding to export",
                    item.span,
                )
            })?;
            names.push(Arc::<str>::from(name));
        }
        // Append to the partially-loaded module's export set. The
        // module is ALWAYS in the registry's "loading" stack at this
        // point — loaded into the table on finish_load. We append
        // exports via a dedicated registry method.
        {
            let mut g = self.modules.inner_lock();
            // The currently-loading module's exports are tracked in a
            // side staging map keyed by path; finalize_load merges
            // the staging into the Module before promoting.
            g.exports_staging
                .entry(current.to_string())
                .or_default()
                .extend(names.iter().cloned());
        }
        Ok(Value::Nil)
    }

    /// Top-level `(require "path" ...)`. Loads the file via the
    /// configured loader, evaluates its contents in a fresh module
    /// context, then imports its exports into the calling env.
    ///
    /// Forms supported:
    ///   (require "path")              ; alias = path; binds path/name
    ///   (require "path" :as alias)    ; binds alias/name
    ///   (require "path" :refer (...)) ; binds bare names; alias also bound
    fn eval_require(&mut self, form: &Spanned, host: &mut H) -> Result<Value> {
        let items = form.as_list().unwrap_or(&[]);
        let span = form.span;
        if items.len() < 2 {
            return Err(EvalError::bad_form(
                "require",
                "expected (require \"path\" [:as alias] [:refer (...)])",
                span,
            ));
        }
        let path: Arc<str> = match items[1].as_string() {
            Some(s) => Arc::from(s),
            None => {
                return Err(EvalError::bad_form(
                    "require",
                    "first arg must be a string path",
                    items[1].span,
                ))
            }
        };

        // Parse optional :as alias / :refer (names) trailing kwargs.
        let mut alias: Option<Arc<str>> = None;
        let mut refer: Option<Vec<Arc<str>>> = None;
        let mut i = 2usize;
        while i < items.len() {
            let kw = items[i].as_keyword().ok_or_else(|| {
                EvalError::bad_form(
                    "require",
                    "expected keyword (:as / :refer) after path",
                    items[i].span,
                )
            })?;
            let val = items.get(i + 1).ok_or_else(|| {
                EvalError::bad_form("require", "keyword without value", items[i].span)
            })?;
            match kw {
                "as" => {
                    alias = Some(Arc::from(val.as_symbol().ok_or_else(|| {
                        EvalError::bad_form("require", ":as needs a symbol alias", val.span)
                    })?));
                }
                "refer" => {
                    let names_list = val.as_list().ok_or_else(|| {
                        EvalError::bad_form(
                            "require",
                            ":refer needs a parenthesized list of symbols",
                            val.span,
                        )
                    })?;
                    let mut names = Vec::with_capacity(names_list.len());
                    for n in names_list {
                        names.push(Arc::<str>::from(n.as_symbol().ok_or_else(|| {
                            EvalError::bad_form(
                                "require",
                                ":refer list must contain symbols only",
                                n.span,
                            )
                        })?));
                    }
                    refer = Some(names);
                }
                other => {
                    return Err(EvalError::bad_form(
                        "require",
                        format!("unknown require option :{other}"),
                        items[i].span,
                    ));
                }
            }
            i += 2;
        }

        // Load + evaluate the module if it's not already cached.
        if !self.modules.has(&path) {
            self.load_module(&path, span, host)?;
        }
        let module = self
            .modules
            .get(&path)
            .ok_or_else(|| EvalError::native_fn("require", "module disappeared after load", span))?;

        // Import bindings into the calling env.
        let chosen_alias = alias.unwrap_or_else(|| path.clone());
        for name in &module.exports {
            let value = module
                .bindings
                .get(name)
                .cloned()
                .unwrap_or(Value::Nil);
            let qualified: Arc<str> = Arc::from(format!("{chosen_alias}/{name}"));
            self.globals.define(qualified, value);
        }
        if let Some(names) = refer {
            for name in names {
                if let Some(value) = module.bindings.get(&name) {
                    if module.exports.contains(&name) {
                        self.globals.define(name.clone(), value.clone());
                    } else {
                        return Err(EvalError::User {
                            value: error_value("not-exported", &format!(
                                "{path} does not export {name}"
                            )),
                            at: span,
                        });
                    }
                } else {
                    return Err(EvalError::User {
                        value: error_value("not-defined", &format!(
                            "{path} does not define {name}"
                        )),
                        at: span,
                    });
                }
            }
        }
        Ok(Value::Nil)
    }

    /// Drive the load of a single module: read source via loader,
    /// register on the load stack (cycle detect), evaluate every form
    /// against a fresh global env owned by THIS interpreter (so the
    /// module sees the same primitives + macros), capture the bindings
    /// that ended up in `globals` after eval, and finalize.
    fn load_module(&mut self, path: &str, span: Span, host: &mut H) -> Result<()> {
        // Cycle detect.
        self.modules
            .begin_load(path)
            .map_err(|e| module_error_to_eval(e, span))?;

        // Read source.
        let source = match self.loader.load(path) {
            Ok(s) => s,
            Err(e) => {
                self.modules.abort_load(path);
                return Err(module_error_to_eval(e, span));
            }
        };

        // Parse.
        let forms = match tatara_lisp::read_spanned(&source) {
            Ok(f) => f,
            Err(e) => {
                self.modules.abort_load(path);
                return Err(EvalError::Reader(e));
            }
        };

        // Save + swap module-context state. We isolate the module's
        // bindings by snapshotting the globals env, evaluating into a
        // FRESH env that inherits the host primitives, then restoring.
        let saved_globals = std::mem::replace(&mut self.globals, Env::new());
        // Re-install primitives into the fresh env: every NativeFn
        // binding from the saved env is copied (the registry behind
        // them is unchanged).
        for (name, value) in saved_globals.iter_top_level() {
            // Only carry NativeFn / Closure bindings forward — these
            // are the primitive surface. The module's user-defined
            // values get isolated.
            if matches!(value, Value::NativeFn(_) | Value::Closure(_)) {
                self.globals.define(name.clone(), value.clone());
            }
        }
        let saved_current = self.current_module.replace(Arc::from(path));

        // Evaluate every form. On error, restore + propagate.
        let mut eval_err: Option<EvalError> = None;
        for f in &forms {
            // Re-enter eval_top_form so nested defmacro / require
            // works recursively. (defmacro inside a module is fine;
            // require chains are how libraries depend on each other.)
            if let Err(e) = self.eval_top_form(f, host) {
                eval_err = Some(e);
                break;
            }
        }

        // Snapshot module's bindings + exports BEFORE restoring globals.
        let module_globals = std::mem::replace(&mut self.globals, saved_globals);
        self.current_module = saved_current;

        if let Some(e) = eval_err {
            self.modules.abort_load(path);
            return Err(e);
        }

        // Build the Module from the captured env's top-level bindings
        // + the staged export set.
        let mut module = Module::new(path);
        for (name, value) in module_globals.iter_top_level() {
            // Skip primitives that we re-inherited. We want only the
            // module's OWN definitions.
            if !matches!(value, Value::NativeFn(_)) {
                module.define(name.clone(), value.clone());
            }
        }
        // Apply staged exports.
        let staged = {
            let mut g = self.modules.inner_lock();
            g.exports_staging
                .remove(path)
                .unwrap_or_default()
        };
        for n in staged {
            module.add_export(n);
        }
        self.modules.finish_load(module);
        Ok(())
    }

    /// Fully expand a form: walk the tree; whenever the head of a list
    /// is a registered macro, evaluate the macro body (a regular Lisp
    /// program) at expansion time, convert the resulting Value back to
    /// a Spanned tree, and recurse — the expansion may itself contain
    /// further macro calls.
    ///
    /// This is the CL/Racket macro model: the macro body has full access
    /// to every primitive and library function, can compute over its
    /// argument source forms (which arrive as Lisp data structures —
    /// lists of symbols, etc.), and produces code as data.
    pub fn fully_expand(&mut self, form: &Spanned, host: &mut H) -> Result<Spanned> {
        // Fast path: no macros registered — nothing to expand.
        if self.expander.is_empty() {
            return Ok(form.clone());
        }
        self.expand_recursive(form, host)
    }

    fn expand_recursive(&mut self, form: &Spanned, host: &mut H) -> Result<Spanned> {
        match &form.form {
            SpannedForm::List(items) if !items.is_empty() => {
                if let Some(head) = items[0].as_symbol() {
                    if self.expander.has(head) {
                        // Macro call. Expand by running the body, then
                        // recurse on the result (it may itself be a
                        // macro call or contain nested macro calls).
                        let expanded =
                            self.expand_macro_call(head, &items[1..], form.span, host)?;
                        return self.expand_recursive(&expanded, host);
                    }
                }
                // Not a macro call — recurse into children to catch
                // nested macros.
                let mut out = Vec::with_capacity(items.len());
                for child in items {
                    out.push(self.expand_recursive(child, host)?);
                }
                Ok(Spanned::new(form.span, SpannedForm::List(out)))
            }
            SpannedForm::Quote(_) => {
                // Inside a `'expr`, expr is data — don't expand inside.
                Ok(form.clone())
            }
            SpannedForm::Quasiquote(inner) => {
                // Inside a `\`expr`, only unquoted subforms get expanded.
                Ok(Spanned::new(
                    form.span,
                    SpannedForm::Quasiquote(Box::new(self.expand_inside_quasiquote(inner, host)?)),
                ))
            }
            // Atoms, Nil, bare Unquote/UnquoteSplice — pass through.
            _ => Ok(form.clone()),
        }
    }

    fn expand_inside_quasiquote(&mut self, form: &Spanned, host: &mut H) -> Result<Spanned> {
        match &form.form {
            SpannedForm::Unquote(inner) => Ok(Spanned::new(
                form.span,
                SpannedForm::Unquote(Box::new(self.expand_recursive(inner, host)?)),
            )),
            SpannedForm::UnquoteSplice(inner) => Ok(Spanned::new(
                form.span,
                SpannedForm::UnquoteSplice(Box::new(self.expand_recursive(inner, host)?)),
            )),
            SpannedForm::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.expand_inside_quasiquote(item, host)?);
                }
                Ok(Spanned::new(form.span, SpannedForm::List(out)))
            }
            _ => Ok(form.clone()),
        }
    }

    /// Expand a single macro call: bind macro params to lowered Value
    /// representations of the source-form args, evaluate the body in
    /// the live interpreter, and lift the result Value back to Spanned.
    fn expand_macro_call(
        &mut self,
        macro_name: &str,
        args: &[Spanned],
        call_span: Span,
        host: &mut H,
    ) -> Result<Spanned> {
        // Take a clone of the def — we'll use it without holding the
        // expander borrow across an eval call.
        let def: MacroDef = self
            .expander
            .get_macro(macro_name)
            .cloned()
            .ok_or_else(|| {
                EvalError::native_fn(
                    Arc::<str>::from(macro_name),
                    "macro disappeared during expansion",
                    call_span,
                )
            })?;

        // Lift the body Sexp (which has no spans) to a Spanned tree
        // stamped with the call site. Errors inside the body will
        // appear at the macro call site — the right behavior for
        // user-facing diagnostics.
        let body_spanned = Spanned::from_sexp_at(&def.body, call_span);

        // Expand any macros INSIDE the body before evaluation. This is
        // what lets a macro use other macros (`dolist`, `when-let`,
        // helper macros from stdlib) in its expansion logic. Without
        // this pass, the body's eval would hit those forms as plain
        // function calls and fail.
        let body_expanded = self.fully_expand(&body_spanned, host)?;

        // Build the macro-time environment: capture globals, push a
        // frame for the macro params.
        let mut macro_env = self.globals.clone();
        macro_env.push();
        bind_macro_args(&mut macro_env, &def.name, &def.params, args, call_span)?;

        // Evaluate the body in the macro env using the live interpreter
        // — every primitive, every library fn is in scope.
        let result = eval_in(
            &mut macro_env,
            &self.registry,
            &self.expander,
            &body_expanded,
            host,
        )?;

        // Convert the resulting Value back to a Spanned form. Anything
        // that can't be lifted (closure, native fn, foreign) is a user
        // error in the macro.
        value_to_spanned(&result, call_span).map_err(|reason| {
            EvalError::native_fn(
                Arc::<str>::from(format!("macro {macro_name}")),
                reason,
                call_span,
            )
        })
    }

    /// Borrow the macro expander. Embedders may register macros directly
    /// (e.g. preloaded standard library) without reading them from source.
    pub fn expander(&self) -> &SpannedExpander {
        &self.expander
    }

    /// Mutable access to the expander — for preloading macros via
    /// `try_register_macro` from a separately-read form list, or clearing
    /// the registry.
    pub fn expander_mut(&mut self) -> &mut SpannedExpander {
        &mut self.expander
    }

    /// Look up a symbol in the global env.
    pub fn lookup_global(&self, name: &str) -> Option<Value> {
        self.globals.lookup(name)
    }

    /// Bind a value in the global env.
    pub fn define_global(&mut self, name: impl Into<Arc<str>>, value: Value) {
        self.globals.define(name, value);
    }

    /// Borrow the globals env. Used by the VM to snapshot at closure
    /// creation time.
    pub fn globals_snapshot(&self) -> &Env {
        &self.globals
    }

    /// External entry point: apply a callable `Value` (closure or
    /// native fn) with `args`. Wraps the internal `apply_external` so
    /// the VM can dispatch to the tree-walker for non-VM callables.
    pub fn apply_external_value(
        &mut self,
        callee: &Value,
        args: Vec<Value>,
        host: &mut H,
        call_span: Span,
    ) -> Result<Value> {
        apply_external(callee, args, call_span, &self.registry, &self.expander, host)
    }

    /// Compile + execute a parsed program through the bytecode VM.
    /// Top-level `defmacro` forms register into the persistent
    /// expander (same as `eval_program`); every other form is
    /// macro-expanded in place, then a fresh `Chunk` is compiled and
    /// run. This is the opt-in fast path; `eval_program` remains the
    /// authoritative tree-walker. Returns the value of the last form.
    pub fn eval_program_vm(&mut self, forms: &[Spanned], host: &mut H) -> Result<Value> {
        let mut expanded: Vec<Spanned> = Vec::with_capacity(forms.len());
        for form in forms {
            if self.expander.try_register_macro(form)? {
                continue;
            }
            expanded.push(self.fully_expand(form, host)?);
        }
        let chunk = crate::vm::compile_program(&expanded).map_err(|e| match e {
            crate::vm::CompileError::Bad { at, message } => {
                EvalError::bad_form(Arc::<str>::from("vm:compile"), message, at)
            }
        })?;
        let mut vm = crate::vm::Vm::new();
        vm.run(&chunk, self, host).map_err(|e| match e {
            crate::vm::VmError::Eval(inner) => inner,
            other => EvalError::native_fn(Arc::<str>::from("vm"), format!("{other}"), Span::synthetic()),
        })
    }

    // ── Typed registration helpers ──────────────────────────────────

    /// Register a 0-arity native fn with typed return value.
    pub fn register_typed0<R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        R: IntoValue + 'static,
        F: Fn(&mut H) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(0),
            move |_args: &[Value], host: &mut H, _sp| f(host).map(IntoValue::into_value),
        );
    }

    /// Register a 1-arity native fn with typed arg + return.
    pub fn register_typed1<A, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(1),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                f(host, a).map(IntoValue::into_value)
            },
        );
    }

    /// Register a 2-arity native fn with typed args + return.
    pub fn register_typed2<A, B, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        B: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A, B) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(2),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                let b = B::from_value(&args[1], sp)?;
                f(host, a, b).map(IntoValue::into_value)
            },
        );
    }

    /// Register a 3-arity native fn with typed args + return.
    pub fn register_typed3<A, B, C, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        B: FromValue + 'static,
        C: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A, B, C) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(3),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                let b = B::from_value(&args[1], sp)?;
                let c = C::from_value(&args[2], sp)?;
                f(host, a, b, c).map(IntoValue::into_value)
            },
        );
    }

    /// Register a 4-arity native fn with typed args + return.
    pub fn register_typed4<A, B, C, D, R, F>(&mut self, name: impl Into<Arc<str>>, f: F)
    where
        A: FromValue + 'static,
        B: FromValue + 'static,
        C: FromValue + 'static,
        D: FromValue + 'static,
        R: IntoValue + 'static,
        F: Fn(&mut H, A, B, C, D) -> Result<R> + Send + Sync + 'static,
    {
        self.register_fn(
            name,
            Arity::Exact(4),
            move |args: &[Value], host: &mut H, sp| {
                let a = A::from_value(&args[0], sp)?;
                let b = B::from_value(&args[1], sp)?;
                let c = C::from_value(&args[2], sp)?;
                let d = D::from_value(&args[3], sp)?;
                f(host, a, b, c, d).map(IntoValue::into_value)
            },
        );
    }
}

impl<H: 'static> Default for Interpreter<H> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Core recursive evaluator ──────────────────────────────────────────

/// Evaluate `form` against `env`, resolving native fns via `registry`.
/// Mutates `env` for `define` / `set!` / body frame push+pop.
pub(crate) fn eval_in<H: 'static>(
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    form: &Spanned,
    host: &mut H,
) -> Result<Value> {
    match &form.form {
        SpannedForm::Nil => Ok(Value::Nil),
        SpannedForm::Atom(a) => eval_atom(a, form.span, env),
        SpannedForm::Quote(inner) => Ok(quoted_value(inner)),
        SpannedForm::Quasiquote(inner) => quasiquote_eval(inner, env, registry, expander, host),
        SpannedForm::Unquote(_) | SpannedForm::UnquoteSplice(_) => Err(EvalError::bad_form(
            "unquote",
            "unquote outside of quasiquote",
            form.span,
        )),
        SpannedForm::List(items) => {
            if items.is_empty() {
                return Ok(Value::Nil);
            }
            // Head may be a special-form keyword, a symbol that resolves
            // to a callable, or an arbitrary expression that evaluates
            // to a callable.
            if let Some(head_sym) = items[0].as_symbol() {
                if let Some(sf) = SpecialForm::from_symbol(head_sym) {
                    return eval_special(sf, items, form.span, env, registry, expander, host);
                }
            }
            eval_application(items, form.span, env, registry, expander, host)
        }
    }
}

fn eval_atom(a: &Atom, span: Span, env: &Env) -> Result<Value> {
    match a {
        Atom::Symbol(name) => env
            .lookup(name)
            .ok_or_else(|| EvalError::unbound(name.as_str(), span)),
        Atom::Keyword(s) => Ok(Value::Keyword(Arc::from(s.as_str()))),
        Atom::Str(s) => Ok(Value::Str(Arc::from(s.as_str()))),
        Atom::Int(n) => Ok(Value::Int(*n)),
        Atom::Float(n) => Ok(Value::Float(*n)),
        Atom::Bool(b) => Ok(Value::Bool(*b)),
    }
}

/// `'x` (Quote node from the reader) — yields the runtime value of x
/// without evaluation. Symbol → Value::Symbol; list → Value::List of
/// lowered children. Same semantics as the explicit `(quote x)`.
fn quoted_value(inner: &Spanned) -> Value {
    crate::code::spanned_to_value(inner)
}

/// Evaluate a quasiquoted form — unlike `quote`, `,expr` inside the form
/// is evaluated and substituted, and `,@expr` splices the evaluated list
/// into the enclosing list. Atoms lower to their runtime `Value`
/// equivalents (Symbol → Value::Symbol, etc.). Nested quasiquote is not
/// supported in v1 — it is returned as an opaque `Value::Sexp` literal.
fn quasiquote_eval<H: 'static>(
    form: &Spanned,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    match &form.form {
        SpannedForm::Unquote(inner) => eval_in(env, registry, expander, inner, host),
        SpannedForm::UnquoteSplice(_) => Err(EvalError::bad_form(
            "unquote-splice",
            "`,@` only valid directly inside a list",
            form.span,
        )),
        SpannedForm::List(items) => {
            let mut out: Vec<Value> = Vec::with_capacity(items.len());
            for item in items {
                if let SpannedForm::UnquoteSplice(inner) = &item.form {
                    let v = eval_in(env, registry, expander, inner, host)?;
                    match v {
                        Value::List(xs) => out.extend(xs.iter().cloned()),
                        Value::Nil => {}
                        other => {
                            return Err(EvalError::type_mismatch(
                                "list",
                                other.type_name(),
                                item.span,
                            ))
                        }
                    }
                } else {
                    out.push(quasiquote_eval(item, env, registry, expander, host)?);
                }
            }
            if out.is_empty() {
                Ok(Value::Nil)
            } else {
                Ok(Value::list(out))
            }
        }
        SpannedForm::Nil => Ok(Value::Nil),
        SpannedForm::Atom(a) => Ok(match a {
            Atom::Symbol(s) => Value::Symbol(Arc::from(s.as_str())),
            Atom::Keyword(s) => Value::Keyword(Arc::from(s.as_str())),
            Atom::Str(s) => Value::Str(Arc::from(s.as_str())),
            Atom::Int(n) => Value::Int(*n),
            Atom::Float(n) => Value::Float(*n),
            Atom::Bool(b) => Value::Bool(*b),
        }),
        // Inside quasiquote, an inner `quote` is preserved structurally —
        // we treat it as an opaque literal subtree so downstream consumers
        // can see it as a source form if they care.
        SpannedForm::Quote(_) | SpannedForm::Quasiquote(_) => {
            Ok(Value::Sexp(form.to_sexp(), form.span))
        }
    }
}

// ── Function application ──────────────────────────────────────────────

fn eval_application<H: 'static>(
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    let head_val = eval_in(env, registry, expander, &items[0], host)?;
    let mut args: Vec<Value> = Vec::with_capacity(items.len().saturating_sub(1));
    for arg_form in &items[1..] {
        args.push(eval_in(env, registry, expander, arg_form, host)?);
    }
    apply(&head_val, args, call_span, registry, expander, host)
}

fn apply<H: 'static>(
    callee: &Value,
    args: Vec<Value>,
    call_span: Span,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    match callee {
        Value::NativeFn(nfn) => {
            if nfn.arity.check(args.len()).is_err() {
                return Err(EvalError::ArityMismatch {
                    fn_name: nfn.name.clone(),
                    expected: nfn.arity,
                    got: args.len(),
                    at: call_span,
                });
            }
            let entry = registry.lookup(&nfn.name).ok_or_else(|| {
                EvalError::native_fn(
                    nfn.name.clone(),
                    format!("native fn {} is not registered", nfn.name),
                    call_span,
                )
            })?;
            match &entry.callable {
                FnImpl::Native(f) => f.call(&args, host, call_span),
                FnImpl::Higher(f) => {
                    let caller = Caller { registry, expander };
                    f.call(&args, host, &caller, call_span)
                }
            }
        }
        Value::Closure(c) => call_closure(c.clone(), args, call_span, registry, expander, host),
        // VM-compiled closure flowing into a tree-walker apply path
        // (typically because a native HoF captured the closure as an
        // arg). Lift to a tree-walker-shaped Closure and dispatch.
        // See `CompiledClosure::lift_to_closure` for trade-offs.
        Value::Foreign(any) => {
            if let Some(cc) = any
                .clone()
                .downcast::<crate::vm::run::CompiledClosure>()
                .ok()
            {
                let lifted = cc.lift_to_closure();
                return call_closure(lifted, args, call_span, registry, expander, host);
            }
            Err(EvalError::NotCallable {
                value_kind: callee.type_name(),
                at: call_span,
            })
        }
        other => Err(EvalError::NotCallable {
            value_kind: other.type_name(),
            at: call_span,
        }),
    }
}

// ── Tail-call optimization ────────────────────────────────────────
//
// Tatara-lisp guarantees TCO in the sense Scheme R7RS requires: a
// procedure call in tail position never grows the stack. This is
// implemented as a trampoline driven from `call_closure`.
//
// "Tail position" is the structural notion: the form whose value
// becomes the value of the surrounding form. The tail positions
// supported here:
//
//   * `if` — both branches
//   * `cond` / `when` / `unless` — last form of the matching body
//   * `begin` / `let` / `let*` / `letrec` — last form of the body
//   * `and` / `or` — last form when prior forms didn't short-circuit
//   * Lambda body — last form
//
// `eval_in_tail` mirrors `eval_in` but, for closure-application forms
// in tail position, returns `TailResult::Resume(closure, args)` rather
// than calling `apply`. The outer trampoline in `call_closure` then
// rebinds and loops without consuming a stack frame.

/// Result of tail-position evaluation.
enum TailResult {
    /// Evaluation completed; here is the value.
    Done(Value),
    /// A tail call to a closure that the trampoline should re-enter
    /// rather than recursing into. Carries the closure to invoke,
    /// the already-evaluated arguments, and the call site span for
    /// arity-error attribution.
    Resume(Arc<Closure>, Vec<Value>, Span),
}

/// Tail-position evaluation. Same semantics as `eval_in` for forms
/// that don't yield a closure tail call, but defers closure tail calls
/// to the trampoline.
fn eval_in_tail<H: 'static>(
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    form: &Spanned,
    host: &mut H,
) -> Result<TailResult> {
    match &form.form {
        SpannedForm::List(items) if !items.is_empty() => {
            // Special-form check first.
            if let Some(head_sym) = items[0].as_symbol() {
                if let Some(sf) = SpecialForm::from_symbol(head_sym) {
                    return eval_special_tail(sf, items, form.span, env, registry, expander, host);
                }
            }
            // Function application: evaluate head + args, then either
            // resume (closure) or apply (everything else).
            let head_val = eval_in(env, registry, expander, &items[0], host)?;
            let mut args: Vec<Value> = Vec::with_capacity(items.len().saturating_sub(1));
            for arg_form in &items[1..] {
                args.push(eval_in(env, registry, expander, arg_form, host)?);
            }
            match head_val {
                Value::Closure(c) => Ok(TailResult::Resume(c, args, form.span)),
                _ => apply(&head_val, args, form.span, registry, expander, host)
                    .map(TailResult::Done),
            }
        }
        // Atoms, Quote, Nil — no tail context to exploit; just compute.
        _ => eval_in(env, registry, expander, form, host).map(TailResult::Done),
    }
}

fn eval_special_tail<H: 'static>(
    sf: SpecialForm,
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<TailResult> {
    match sf {
        SpecialForm::If => {
            if items.len() < 3 || items.len() > 4 {
                return eval_special(sf, items, call_span, env, registry, expander, host)
                    .map(TailResult::Done);
            }
            let c = eval_in(env, registry, expander, &items[1], host)?;
            if c.is_truthy() {
                eval_in_tail(env, registry, expander, &items[2], host)
            } else if items.len() == 4 {
                eval_in_tail(env, registry, expander, &items[3], host)
            } else {
                Ok(TailResult::Done(Value::Nil))
            }
        }
        SpecialForm::Begin => {
            let body = &items[1..];
            if body.is_empty() {
                return Ok(TailResult::Done(Value::Nil));
            }
            for form in &body[..body.len() - 1] {
                eval_in(env, registry, expander, form, host)?;
            }
            eval_in_tail(env, registry, expander, body.last().unwrap(), host)
        }
        SpecialForm::When | SpecialForm::Unless => {
            if items.len() < 2 {
                return eval_special(sf, items, call_span, env, registry, expander, host)
                    .map(TailResult::Done);
            }
            let invert = matches!(sf, SpecialForm::Unless);
            let cond = eval_in(env, registry, expander, &items[1], host)?;
            let run = cond.is_truthy() ^ invert;
            if !run {
                return Ok(TailResult::Done(Value::Nil));
            }
            let body = &items[2..];
            if body.is_empty() {
                return Ok(TailResult::Done(Value::Nil));
            }
            for form in &body[..body.len() - 1] {
                eval_in(env, registry, expander, form, host)?;
            }
            eval_in_tail(env, registry, expander, body.last().unwrap(), host)
        }
        SpecialForm::Cond => {
            for clause in &items[1..] {
                let Some(clause_list) = clause.as_list() else {
                    return eval_special(sf, items, call_span, env, registry, expander, host)
                        .map(TailResult::Done);
                };
                if clause_list.is_empty() {
                    return eval_special(sf, items, call_span, env, registry, expander, host)
                        .map(TailResult::Done);
                }
                let is_else = clause_list[0].as_symbol() == Some("else");
                let cond_matches = if is_else {
                    true
                } else {
                    eval_in(env, registry, expander, &clause_list[0], host)?.is_truthy()
                };
                if cond_matches {
                    let body = &clause_list[1..];
                    if body.is_empty() {
                        return Ok(TailResult::Done(Value::Nil));
                    }
                    for form in &body[..body.len() - 1] {
                        eval_in(env, registry, expander, form, host)?;
                    }
                    return eval_in_tail(env, registry, expander, body.last().unwrap(), host);
                }
            }
            Ok(TailResult::Done(Value::Nil))
        }
        SpecialForm::Let | SpecialForm::LetStar | SpecialForm::LetRec => {
            eval_let_family_tail(sf, items, call_span, env, registry, expander, host)
        }
        SpecialForm::And => {
            let exprs = &items[1..];
            if exprs.is_empty() {
                return Ok(TailResult::Done(Value::Bool(true)));
            }
            // All but last: short-circuit.
            for e in &exprs[..exprs.len() - 1] {
                let v = eval_in(env, registry, expander, e, host)?;
                if !v.is_truthy() {
                    return Ok(TailResult::Done(v));
                }
            }
            // Last in tail position.
            eval_in_tail(env, registry, expander, exprs.last().unwrap(), host)
        }
        SpecialForm::Or => {
            let exprs = &items[1..];
            if exprs.is_empty() {
                return Ok(TailResult::Done(Value::Bool(false)));
            }
            for e in &exprs[..exprs.len() - 1] {
                let v = eval_in(env, registry, expander, e, host)?;
                if v.is_truthy() {
                    return Ok(TailResult::Done(v));
                }
            }
            eval_in_tail(env, registry, expander, exprs.last().unwrap(), host)
        }
        SpecialForm::Try => {
            // try/catch is delicate to TCO — preserving the catch
            // handler context across a tail call would require unwinding
            // through Resume. Punt: always run try in non-tail position.
            // Tail position inside the catch handler is fine; the body
            // simply doesn't trampoline a tail call past the try frame.
            sf_try(items, call_span, env, registry, expander, host).map(TailResult::Done)
        }
        SpecialForm::MacroexpandOne => {
            sf_macroexpand(items, call_span, env, registry, expander, host, false)
                .map(TailResult::Done)
        }
        SpecialForm::MacroexpandAll => {
            sf_macroexpand(items, call_span, env, registry, expander, host, true)
                .map(TailResult::Done)
        }
        SpecialForm::Delay => sf_delay(items, call_span, env).map(TailResult::Done),
        SpecialForm::Eval => {
            sf_eval(items, call_span, env, registry, expander, host).map(TailResult::Done)
        }
        // Non-tail forms: just evaluate normally.
        _ => {
            eval_special(sf, items, call_span, env, registry, expander, host).map(TailResult::Done)
        }
    }
}

/// Tail-aware evaluator for `let` / `let*` / `letrec`. Mirrors the
/// non-tail versions in `sf_let` / `sf_let_star` / `sf_letrec` but uses
/// `eval_in_tail` for the body's last form.
fn eval_let_family_tail<H: 'static>(
    sf: SpecialForm,
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<TailResult> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            match sf {
                SpecialForm::Let => "let",
                SpecialForm::LetStar => "let*",
                SpecialForm::LetRec => "letrec",
                _ => "let-family",
            },
            "expected ((name expr)...) body...",
            call_span,
        ));
    }
    let bindings = parse_binding_list(
        &items[1],
        match sf {
            SpecialForm::Let => "let",
            SpecialForm::LetStar => "let*",
            SpecialForm::LetRec => "letrec",
            _ => "let-family",
        },
    )?;

    match sf {
        SpecialForm::Let => {
            let mut values = Vec::with_capacity(bindings.len());
            for (_, expr) in &bindings {
                values.push(eval_in(env, registry, expander, expr, host)?);
            }
            env.push();
            for ((name, _), val) in bindings.into_iter().zip(values) {
                env.define(name, val);
            }
        }
        SpecialForm::LetStar => {
            env.push();
            for (name, expr) in bindings {
                let v = eval_in(env, registry, expander, expr, host)?;
                env.define(name, v);
            }
        }
        SpecialForm::LetRec => {
            env.push();
            for (name, _) in &bindings {
                env.define(name.clone(), Value::Nil);
            }
            for (name, expr) in &bindings {
                let v = eval_in(env, registry, expander, expr, host)?;
                env.define(name.clone(), v);
            }
        }
        _ => unreachable!(),
    }

    let body = &items[2..];
    let result = if body.is_empty() {
        Ok(TailResult::Done(Value::Nil))
    } else {
        for form in &body[..body.len() - 1] {
            if let Err(e) = eval_in(env, registry, expander, form, host) {
                env.pop();
                return Err(e);
            }
        }
        eval_in_tail(env, registry, expander, body.last().unwrap(), host)
    };
    env.pop();
    result
}

/// External entry point for `Caller::apply_value` — the higher-order
/// primitive needs to invoke a callable Value back into the eval loop.
/// This is the same `apply` function above; it is exposed `pub(crate)`
/// at function visibility so the FFI module can reach it without
/// publishing the rest of the eval internals.
pub(crate) fn apply_external<H: 'static>(
    callee: &Value,
    args: Vec<Value>,
    call_span: Span,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    apply(callee, args, call_span, registry, expander, host)
}

/// Bind macro parameters onto the macro-time env. Required params each
/// take one arg, lowered Spanned→Value. The optional `&rest` param
/// takes the remainder as a `Value::List` of lowered args.
fn bind_macro_args(
    env: &mut Env,
    macro_name: &str,
    params: &[Param],
    args: &[Spanned],
    call_span: Span,
) -> Result<()> {
    let mut cursor = 0usize;
    for p in params {
        match p {
            Param::Required(name) => {
                let arg = args.get(cursor).ok_or_else(|| {
                    EvalError::native_fn(
                        Arc::<str>::from(format!("macro {macro_name}")),
                        format!("missing required arg: {name}"),
                        call_span,
                    )
                })?;
                env.define(Arc::<str>::from(name.as_str()), spanned_to_value(arg));
                cursor += 1;
            }
            Param::Rest(name) => {
                let rest: Vec<Value> = args
                    .get(cursor..)
                    .unwrap_or(&[])
                    .iter()
                    .map(spanned_to_value)
                    .collect();
                env.define(Arc::<str>::from(name.as_str()), Value::list(rest));
                cursor = args.len();
            }
        }
    }
    Ok(())
}

/// Apply a closure to arguments. Implements TCO: if the body's last
/// form is a tail call to another closure, the trampoline reuses the
/// stack frame instead of recursing. Self-recursion and mutual
/// recursion both bottom out into a loop.
fn call_closure<H: 'static>(
    closure: Arc<Closure>,
    args: Vec<Value>,
    call_span: Span,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    let mut current = closure;
    let mut current_args = args;
    let mut current_span = call_span;
    loop {
        // Arity check.
        let required = current.params.len();
        let has_rest = current.rest.is_some();
        if !has_rest && current_args.len() != required {
            return Err(EvalError::ArityMismatch {
                fn_name: Arc::from("<closure>"),
                expected: Arity::Exact(required),
                got: current_args.len(),
                at: current_span,
            });
        }
        if has_rest && current_args.len() < required {
            return Err(EvalError::ArityMismatch {
                fn_name: Arc::from("<closure>"),
                expected: Arity::AtLeast(required),
                got: current_args.len(),
                at: current_span,
            });
        }

        // Build the body env: capture closure's lexical scope, push frame,
        // bind params + rest.
        let mut env = current.captured_env.clone();
        env.push();
        for (param, arg) in current.params.iter().zip(current_args.iter()) {
            env.define(param.clone(), arg.clone());
        }
        if let Some(rest_name) = &current.rest {
            let rest_args: Vec<Value> = current_args.iter().skip(required).cloned().collect();
            env.define(rest_name.clone(), Value::list(rest_args));
        }

        // Body: evaluate all but the last normally, then the last in
        // tail position so a tail call can be trampolined.
        let body = &current.body;
        if body.is_empty() {
            return Ok(Value::Nil);
        }
        for body_form in &body[..body.len() - 1] {
            eval_in(&mut env, registry, expander, body_form, host)?;
        }
        match eval_in_tail(&mut env, registry, expander, body.last().unwrap(), host)? {
            TailResult::Done(v) => return Ok(v),
            TailResult::Resume(next, next_args, next_span) => {
                // Tail call: replace state and loop. Drop env (frame
                // popped on next iteration's fresh env).
                current = next;
                current_args = next_args;
                current_span = next_span;
            }
        }
    }
}

// ── Special forms ─────────────────────────────────────────────────────

fn eval_special<H: 'static>(
    sf: SpecialForm,
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    match sf {
        SpecialForm::Quote => sf_quote(items, call_span),
        SpecialForm::Quasiquote => {
            if items.len() != 2 {
                return Err(EvalError::bad_form(
                    "quasiquote",
                    format!("expected 1 arg, got {}", items.len() - 1),
                    call_span,
                ));
            }
            quasiquote_eval(&items[1], env, registry, expander, host)
        }
        SpecialForm::If => sf_if(items, call_span, env, registry, expander, host),
        SpecialForm::Cond => sf_cond(items, call_span, env, registry, expander, host),
        SpecialForm::When => sf_when_unless(items, call_span, env, registry, expander, host, false),
        SpecialForm::Unless => {
            sf_when_unless(items, call_span, env, registry, expander, host, true)
        }
        SpecialForm::Let => sf_let(items, call_span, env, registry, expander, host),
        SpecialForm::LetStar => sf_let_star(items, call_span, env, registry, expander, host),
        SpecialForm::LetRec => sf_letrec(items, call_span, env, registry, expander, host),
        SpecialForm::Lambda => sf_lambda(items, call_span, env),
        SpecialForm::Define => sf_define(items, call_span, env, registry, expander, host),
        SpecialForm::Set => sf_set(items, call_span, env, registry, expander, host),
        SpecialForm::Begin => sf_begin(&items[1..], env, registry, expander, host),
        SpecialForm::And => sf_and(&items[1..], env, registry, expander, host),
        SpecialForm::Or => sf_or(&items[1..], env, registry, expander, host),
        SpecialForm::Not => sf_not(items, call_span, env, registry, expander, host),
        SpecialForm::Try => sf_try(items, call_span, env, registry, expander, host),
        SpecialForm::MacroexpandOne => {
            sf_macroexpand(items, call_span, env, registry, expander, host, false)
        }
        SpecialForm::MacroexpandAll => {
            sf_macroexpand(items, call_span, env, registry, expander, host, true)
        }
        SpecialForm::Delay => sf_delay(items, call_span, env),
        SpecialForm::Eval => sf_eval(items, call_span, env, registry, expander, host),
        SpecialForm::Provide | SpecialForm::Require => Err(EvalError::bad_form(
            if matches!(sf, SpecialForm::Provide) { "provide" } else { "require" },
            "module-system forms are only valid at top level — wrap your call in (eval (quote ...)) if you really need it dynamic",
            call_span,
        )),
    }
}

/// Extract the head-symbol of a list form, or `None` if `form` isn't a
/// list whose head is a symbol. Used by the top-level dispatcher to
/// recognize module-system forms before macroexpansion.
fn head_symbol(form: &Spanned) -> Option<&str> {
    let SpannedForm::List(items) = &form.form else {
        return None;
    };
    items.first().and_then(Spanned::as_symbol)
}

/// Build a `Value::Error` with the given tag + message.
fn error_value(tag: &str, message: &str) -> Value {
    Value::Error(Arc::new(ErrorObj {
        tag: Arc::from(tag),
        message: Arc::from(message),
        data: Vec::new(),
    }))
}

/// Convert a `ModuleError` to the `EvalError::User` carrying a
/// `Value::Error`. This way module-system failures can be `(catch ...)`-ed
/// like any other thrown error.
fn module_error_to_eval(e: ModuleError, span: Span) -> EvalError {
    let (tag, message) = match &e {
        ModuleError::NotFound(_) => ("module-not-found", e.to_string()),
        ModuleError::Circular { .. } => ("circular-require", e.to_string()),
        ModuleError::NotExported(_, _) => ("not-exported", e.to_string()),
    };
    EvalError::User {
        value: error_value(tag, &message),
        at: span,
    }
}

fn sf_quote(items: &[Spanned], span: Span) -> Result<Value> {
    if items.len() != 2 {
        return Err(EvalError::bad_form(
            "quote",
            format!("expected 1 arg, got {}", items.len() - 1),
            span,
        ));
    }
    // Scheme / Clojure semantics: (quote x) returns the runtime
    // structural value of x. A bare symbol becomes Value::Symbol; a
    // list becomes Value::List of recursively-lowered items; etc.
    // This is what makes (car '(a b c)) return the symbol `a` —
    // exactly what users expect from a Lisp.
    Ok(crate::code::spanned_to_value(&items[1]))
}

fn sf_if<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 || items.len() > 4 {
        return Err(EvalError::bad_form(
            "if",
            format!("expected (if c t [e]), got {} subforms", items.len()),
            span,
        ));
    }
    let c = eval_in(env, registry, expander, &items[1], host)?;
    if c.is_truthy() {
        eval_in(env, registry, expander, &items[2], host)
    } else if items.len() == 4 {
        eval_in(env, registry, expander, &items[3], host)
    } else {
        Ok(Value::Nil)
    }
}

fn sf_cond<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    for clause in &items[1..] {
        let Some(clause_list) = clause.as_list() else {
            return Err(EvalError::bad_form(
                "cond",
                "clause must be a list",
                clause.span,
            ));
        };
        if clause_list.is_empty() {
            return Err(EvalError::bad_form("cond", "empty clause", clause.span));
        }
        let is_else = clause_list[0].as_symbol() == Some("else");
        let cond_matches = if is_else {
            true
        } else {
            let v = eval_in(env, registry, expander, &clause_list[0], host)?;
            v.is_truthy()
        };
        if cond_matches {
            let mut last = Value::Nil;
            for expr in &clause_list[1..] {
                last = eval_in(env, registry, expander, expr, host)?;
            }
            return Ok(last);
        }
    }
    // No clause matched.
    let _ = span;
    Ok(Value::Nil)
}

fn sf_when_unless<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
    invert: bool,
) -> Result<Value> {
    if items.len() < 2 {
        return Err(EvalError::bad_form(
            if invert { "unless" } else { "when" },
            "need a test",
            span,
        ));
    }
    let cond = eval_in(env, registry, expander, &items[1], host)?;
    let run = cond.is_truthy() ^ invert;
    if run {
        let mut last = Value::Nil;
        for expr in &items[2..] {
            last = eval_in(env, registry, expander, expr, host)?;
        }
        Ok(last)
    } else {
        Ok(Value::Nil)
    }
}

/// Parse a `((name expr) ...)` binding list into `[(name, &expr_spanned)]`.
fn parse_binding_list<'a>(
    list: &'a Spanned,
    form_name: &'static str,
) -> Result<Vec<(Arc<str>, &'a Spanned)>> {
    let bindings = list
        .as_list()
        .ok_or_else(|| EvalError::bad_form(form_name, "bindings must be a list", list.span))?;
    let mut out = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let pair = binding.as_list().ok_or_else(|| {
            EvalError::bad_form(form_name, "each binding must be (name expr)", binding.span)
        })?;
        if pair.len() != 2 {
            return Err(EvalError::bad_form(
                form_name,
                "binding must be exactly (name expr)",
                binding.span,
            ));
        }
        let name = pair[0].as_symbol().ok_or_else(|| {
            EvalError::bad_form(form_name, "binding name must be a symbol", pair[0].span)
        })?;
        out.push((Arc::<str>::from(name), &pair[1]));
    }
    Ok(out)
}

fn sf_let<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "let",
            "expected (let ((name expr)...) body...)",
            span,
        ));
    }
    let bindings = parse_binding_list(&items[1], "let")?;
    // Parallel semantics: evaluate all RHS in the *outer* env, then
    // extend with new frame.
    let mut values = Vec::with_capacity(bindings.len());
    for (_, expr) in &bindings {
        values.push(eval_in(env, registry, expander, expr, host)?);
    }
    env.push();
    for ((name, _), val) in bindings.into_iter().zip(values) {
        env.define(name, val);
    }
    let result = eval_body(&items[2..], env, registry, expander, host);
    env.pop();
    result
}

fn sf_let_star<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "let*",
            "expected (let* ((name expr)...) body...)",
            span,
        ));
    }
    let bindings = parse_binding_list(&items[1], "let*")?;
    env.push();
    for (name, expr) in bindings {
        let v = eval_in(env, registry, expander, expr, host)?;
        env.define(name, v);
    }
    let result = eval_body(&items[2..], env, registry, expander, host);
    env.pop();
    result
}

fn sf_letrec<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "letrec",
            "expected (letrec ((name expr)...) body...)",
            span,
        ));
    }
    let bindings = parse_binding_list(&items[1], "letrec")?;
    env.push();
    // Pre-bind each name to Nil so RHS can self-reference (and cross-
    // reference). Then eval each RHS in order and rebind.
    for (name, _) in &bindings {
        env.define(name.clone(), Value::Nil);
    }
    for (name, expr) in &bindings {
        let v = eval_in(env, registry, expander, expr, host)?;
        env.define(name.clone(), v);
    }
    let result = eval_body(&items[2..], env, registry, expander, host);
    env.pop();
    result
}

fn eval_body<H: 'static>(
    body: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    let mut last = Value::Nil;
    for form in body {
        last = eval_in(env, registry, expander, form, host)?;
    }
    Ok(last)
}

fn sf_lambda(items: &[Spanned], span: Span, env: &Env) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "lambda",
            "expected (lambda (params...) body...)",
            span,
        ));
    }
    // Empty `()` source parses as Nil, not List([]); accept both as
    // "no parameters". Anything else must be a List.
    let param_list: &[Spanned] = match &items[1].form {
        SpannedForm::Nil => &[],
        SpannedForm::List(xs) => xs.as_slice(),
        _ => {
            return Err(EvalError::bad_form(
                "lambda",
                "params must be a list",
                items[1].span,
            ))
        }
    };
    let (params, rest) = parse_lambda_params(param_list, items[1].span)?;
    let body = items[2..].to_vec();
    Ok(Value::Closure(Arc::new(Closure {
        params,
        rest,
        body,
        captured_env: env.clone(),
        source: span,
    })))
}

fn parse_lambda_params(list: &[Spanned], span: Span) -> Result<(Vec<Arc<str>>, Option<Arc<str>>)> {
    let mut params = Vec::new();
    let mut rest = None;
    let mut i = 0;
    while i < list.len() {
        let s = list[i]
            .as_symbol()
            .ok_or_else(|| EvalError::bad_form("lambda", "param must be a symbol", list[i].span))?;
        if s == "&rest" {
            let name = list
                .get(i + 1)
                .and_then(Spanned::as_symbol)
                .ok_or_else(|| EvalError::bad_form("lambda", "&rest needs a name", span))?;
            rest = Some(Arc::<str>::from(name));
            if i + 2 != list.len() {
                return Err(EvalError::bad_form(
                    "lambda",
                    "&rest must be the last param",
                    span,
                ));
            }
            break;
        }
        params.push(Arc::<str>::from(s));
        i += 1;
    }
    Ok((params, rest))
}

/// `(define name expr)` or `(define (name params...) body...)`
fn sf_define<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "define",
            "expected (define name expr) or (define (name args) body)",
            span,
        ));
    }
    match &items[1].form {
        SpannedForm::Atom(Atom::Symbol(name)) => {
            let v = eval_in(env, registry, expander, &items[2], host)?;
            env.define(Arc::<str>::from(name.as_str()), v);
            Ok(Value::Nil)
        }
        SpannedForm::List(head_list) => {
            if head_list.is_empty() {
                return Err(EvalError::bad_form(
                    "define",
                    "empty (name args) list",
                    items[1].span,
                ));
            }
            let name = head_list[0].as_symbol().ok_or_else(|| {
                EvalError::bad_form(
                    "define",
                    "first item in (name args) must be a symbol",
                    head_list[0].span,
                )
            })?;
            let (params, rest) = parse_lambda_params(&head_list[1..], items[1].span)?;
            let body = items[2..].to_vec();
            let closure = Arc::new(Closure {
                params,
                rest,
                body,
                captured_env: env.clone(),
                source: span,
            });
            env.define(Arc::<str>::from(name), Value::Closure(closure));
            Ok(Value::Nil)
        }
        _ => Err(EvalError::bad_form(
            "define",
            "second form must be a symbol or (name args) list",
            items[1].span,
        )),
    }
}

fn sf_set<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() != 3 {
        return Err(EvalError::bad_form(
            "set!",
            "expected (set! name expr)",
            span,
        ));
    }
    let name = items[1]
        .as_symbol()
        .ok_or_else(|| EvalError::bad_form("set!", "first arg must be a symbol", items[1].span))?;
    let v = eval_in(env, registry, expander, &items[2], host)?;
    if env.set(name, v) {
        Ok(Value::Nil)
    } else {
        Err(EvalError::unbound(name, items[1].span))
    }
}

fn sf_begin<H: 'static>(
    body: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    eval_body(body, env, registry, expander, host)
}

fn sf_and<H: 'static>(
    exprs: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    let mut last = Value::Bool(true);
    for e in exprs {
        last = eval_in(env, registry, expander, e, host)?;
        if !last.is_truthy() {
            return Ok(last);
        }
    }
    Ok(last)
}

fn sf_or<H: 'static>(
    exprs: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    let mut last = Value::Bool(false);
    for e in exprs {
        last = eval_in(env, registry, expander, e, host)?;
        if last.is_truthy() {
            return Ok(last);
        }
    }
    Ok(last)
}

fn sf_not<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() != 2 {
        return Err(EvalError::bad_form("not", "expected (not x)", span));
    }
    let v = eval_in(env, registry, expander, &items[1], host)?;
    Ok(Value::Bool(!v.is_truthy()))
}

/// `(try body... (catch (binding) handler...))` — evaluate body
/// sequentially. If any form raises an `EvalError::User` (Lisp
/// `(throw ...)`), bind the thrown Value to `binding` and run handler.
/// Other Rust-side errors (type mismatch, arity, etc.) are converted
/// to a `Value::Error` with tag `:runtime` so handlers can also
/// recover from them.
///
/// Form layout:
/// ```text
///   (try
///     body-expr
///     ...
///     (catch (e) handler-body...))
/// ```
/// The catch clause MUST be the last form. There can only be one
/// catch clause. Body forms before it are evaluated in order; the
/// last body form's value (or the handler's value, if caught) is
/// returned.
fn sf_try<H: 'static>(
    items: &[Spanned],
    span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() < 3 {
        return Err(EvalError::bad_form(
            "try",
            "expected (try body... (catch (e) handler...))",
            span,
        ));
    }
    // The last form must be a catch clause.
    let catch_form = items.last().unwrap();
    let catch_list = catch_form.as_list().ok_or_else(|| {
        EvalError::bad_form(
            "try",
            "last form must be (catch (binding) handler...)",
            catch_form.span,
        )
    })?;
    if catch_list.is_empty() || catch_list[0].as_symbol() != Some("catch") {
        return Err(EvalError::bad_form(
            "try",
            "last form must be a (catch ...) clause",
            catch_form.span,
        ));
    }
    if catch_list.len() < 3 {
        return Err(EvalError::bad_form(
            "catch",
            "expected (catch (binding) handler...)",
            catch_form.span,
        ));
    }
    let binding_list = catch_list[1].as_list().ok_or_else(|| {
        EvalError::bad_form(
            "catch",
            "binding must be a 1-element list (e)",
            catch_list[1].span,
        )
    })?;
    if binding_list.len() != 1 {
        return Err(EvalError::bad_form(
            "catch",
            "binding must bind exactly one symbol",
            catch_list[1].span,
        ));
    }
    let binding_name = binding_list[0].as_symbol().ok_or_else(|| {
        EvalError::bad_form("catch", "binding must be a symbol", binding_list[0].span)
    })?;

    let body = &items[1..items.len() - 1];
    let mut last = Value::Nil;
    for form in body {
        match eval_in(env, registry, expander, form, host) {
            Ok(v) => {
                last = v;
            }
            Err(EvalError::User { value, .. }) => {
                return run_catch_handler(
                    binding_name,
                    value,
                    &catch_list[2..],
                    env,
                    registry,
                    expander,
                    host,
                );
            }
            Err(other) => {
                // Convert any other runtime error into a Value::Error
                // so catch can still observe it. Tag :runtime
                // distinguishes from user-thrown errors.
                let value = rust_err_to_value_error(&other);
                return run_catch_handler(
                    binding_name,
                    value,
                    &catch_list[2..],
                    env,
                    registry,
                    expander,
                    host,
                );
            }
        }
    }
    Ok(last)
}

fn run_catch_handler<H: 'static>(
    binding_name: &str,
    error_value: Value,
    handler_body: &[Spanned],
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    env.push();
    env.define(Arc::<str>::from(binding_name), error_value);
    let mut last = Value::Nil;
    for form in handler_body {
        match eval_in(env, registry, expander, form, host) {
            Ok(v) => last = v,
            Err(e) => {
                env.pop();
                return Err(e);
            }
        }
    }
    env.pop();
    Ok(last)
}

/// `(eval form)` — evaluate the runtime Value `form` as code. The
/// argument is itself evaluated first to obtain the form (typically
/// a quoted list). The form is then lifted to Spanned, fully expanded
/// (in case it contains macro calls), and evaluated in the current
/// env. Returns the result.
///
/// Unlocks runtime metaprogramming: `(eval (read-string source))` is
/// the canonical "compile + run from string" pattern.
fn sf_eval<H: 'static>(
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
) -> Result<Value> {
    if items.len() != 2 {
        return Err(EvalError::bad_form(
            "eval",
            "expected (eval form)",
            call_span,
        ));
    }
    let form_value = eval_in(env, registry, expander, &items[1], host)?;
    let form_spanned = crate::code::value_to_spanned(&form_value, call_span)
        .map_err(|reason| EvalError::native_fn(Arc::<str>::from("eval"), reason, call_span))?;
    let expanded = fully_expand_with(&form_spanned, registry, expander, env, host)?;
    eval_in(env, registry, expander, &expanded, host)
}

/// `(delay expr)` — wrap `expr` in a `Value::Promise` whose first
/// `force` evaluates the body once and caches. The body becomes the
/// closure body of a 0-arity lambda capturing the current env, then
/// stored as the promise's pending state.
fn sf_delay(items: &[Spanned], call_span: Span, env: &Env) -> Result<Value> {
    if items.len() != 2 {
        return Err(EvalError::bad_form(
            "delay",
            "expected (delay expr)",
            call_span,
        ));
    }
    let body = vec![items[1].clone()];
    let thunk = Arc::new(Closure {
        params: Vec::new(),
        rest: None,
        body,
        captured_env: env.clone(),
        source: call_span,
    });
    Ok(Value::Promise(Arc::new(std::sync::Mutex::new(
        crate::value::PromiseState::Pending(thunk),
    ))))
}

/// `(macroexpand-1 form)` and `(macroexpand form)` — return the
/// expansion of `form` as a Value. `form` is evaluated to obtain a
/// source-form Value (typically a quoted list); we lift it back to a
/// Spanned, run one (macroexpand-1) or full (macroexpand) expansion,
/// then convert the result Value back.
///
/// Useful for debugging macros — see exactly what the expander
/// produces given a sample input.
fn sf_macroexpand<H: 'static>(
    items: &[Spanned],
    call_span: Span,
    env: &mut Env,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    host: &mut H,
    fully: bool,
) -> Result<Value> {
    if items.len() != 2 {
        return Err(EvalError::bad_form(
            if fully {
                "macroexpand"
            } else {
                "macroexpand-1"
            },
            "expected (macroexpand[-1] form)",
            call_span,
        ));
    }
    // Evaluate the argument to obtain a source-form Value.
    let form_value = eval_in(env, registry, expander, &items[1], host)?;
    // Lift to Spanned so the expander can walk it.
    let form_spanned = crate::code::value_to_spanned(&form_value, call_span).map_err(|reason| {
        EvalError::native_fn(
            Arc::<str>::from(if fully {
                "macroexpand"
            } else {
                "macroexpand-1"
            }),
            reason,
            call_span,
        )
    })?;

    // Build a fresh interpreter-style call into the same expander/registry.
    // We can't recursively call self.fully_expand or self.expand_macro_call
    // here because we don't have &mut Interpreter. Instead, we do the
    // single-step or recursive expansion ourselves via the same
    // primitives that the Interpreter uses.
    let expanded = if fully {
        fully_expand_with(&form_spanned, registry, expander, env, host)?
    } else {
        macroexpand_one(&form_spanned, registry, expander, env, host)?
    };

    Ok(crate::code::spanned_to_value(&expanded))
}

/// Free-function variant of `Interpreter::expand_macro_call`. Takes the
/// state pieces explicitly so it can be called from a special form
/// (where we don't have `&mut Interpreter` available).
fn expand_one_macro_call<H: 'static>(
    macro_name: &str,
    args: &[Spanned],
    call_span: Span,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    parent_env: &Env,
    host: &mut H,
) -> Result<Spanned> {
    let def: MacroDef = expander.get_macro(macro_name).cloned().ok_or_else(|| {
        EvalError::native_fn(
            Arc::<str>::from(macro_name),
            "macro disappeared during expansion",
            call_span,
        )
    })?;
    let body_spanned = Spanned::from_sexp_at(&def.body, call_span);
    // First expand any macros inside the body itself.
    let body_expanded = fully_expand_with(&body_spanned, registry, expander, parent_env, host)?;

    let mut macro_env = parent_env.clone();
    macro_env.push();
    bind_macro_args(&mut macro_env, &def.name, &def.params, args, call_span)?;
    let result = eval_in(&mut macro_env, registry, expander, &body_expanded, host)?;

    crate::code::value_to_spanned(&result, call_span).map_err(|reason| {
        EvalError::native_fn(
            Arc::<str>::from(format!("macro {macro_name}")),
            reason,
            call_span,
        )
    })
}

/// Free-function variant of `Interpreter::fully_expand`. Recursively
/// expands every macro call in the form tree, terminating at fixed
/// point.
fn fully_expand_with<H: 'static>(
    form: &Spanned,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    parent_env: &Env,
    host: &mut H,
) -> Result<Spanned> {
    if expander.is_empty() {
        return Ok(form.clone());
    }
    expand_recursive_with(form, registry, expander, parent_env, host)
}

fn expand_recursive_with<H: 'static>(
    form: &Spanned,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    parent_env: &Env,
    host: &mut H,
) -> Result<Spanned> {
    match &form.form {
        SpannedForm::List(items) if !items.is_empty() => {
            if let Some(head) = items[0].as_symbol() {
                if expander.has(head) {
                    let expanded = expand_one_macro_call(
                        head,
                        &items[1..],
                        form.span,
                        registry,
                        expander,
                        parent_env,
                        host,
                    )?;
                    return expand_recursive_with(&expanded, registry, expander, parent_env, host);
                }
            }
            let mut out = Vec::with_capacity(items.len());
            for child in items {
                out.push(expand_recursive_with(
                    child, registry, expander, parent_env, host,
                )?);
            }
            Ok(Spanned::new(form.span, SpannedForm::List(out)))
        }
        SpannedForm::Quote(_) => Ok(form.clone()),
        SpannedForm::Quasiquote(inner) => Ok(Spanned::new(
            form.span,
            SpannedForm::Quasiquote(Box::new(expand_inside_quasiquote_with(
                inner, registry, expander, parent_env, host,
            )?)),
        )),
        _ => Ok(form.clone()),
    }
}

fn expand_inside_quasiquote_with<H: 'static>(
    form: &Spanned,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    parent_env: &Env,
    host: &mut H,
) -> Result<Spanned> {
    match &form.form {
        SpannedForm::Unquote(inner) => Ok(Spanned::new(
            form.span,
            SpannedForm::Unquote(Box::new(expand_recursive_with(
                inner, registry, expander, parent_env, host,
            )?)),
        )),
        SpannedForm::UnquoteSplice(inner) => Ok(Spanned::new(
            form.span,
            SpannedForm::UnquoteSplice(Box::new(expand_recursive_with(
                inner, registry, expander, parent_env, host,
            )?)),
        )),
        SpannedForm::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(expand_inside_quasiquote_with(
                    item, registry, expander, parent_env, host,
                )?);
            }
            Ok(Spanned::new(form.span, SpannedForm::List(out)))
        }
        _ => Ok(form.clone()),
    }
}

/// One-step macroexpansion: expand ONLY the head call if it's a macro;
/// otherwise return form unchanged. Children are NOT expanded.
fn macroexpand_one<H: 'static>(
    form: &Spanned,
    registry: &FnRegistry<H>,
    expander: &SpannedExpander,
    parent_env: &Env,
    host: &mut H,
) -> Result<Spanned> {
    if let SpannedForm::List(items) = &form.form {
        if let Some(head) = items.first().and_then(Spanned::as_symbol) {
            if expander.has(head) {
                return expand_one_macro_call(
                    head,
                    &items[1..],
                    form.span,
                    registry,
                    expander,
                    parent_env,
                    host,
                );
            }
        }
    }
    Ok(form.clone())
}

/// Convert a Rust-side `EvalError` into a `Value::Error` so a `(catch)`
/// handler can observe runtime errors uniformly with user-thrown ones.
fn rust_err_to_value_error(err: &EvalError) -> Value {
    use crate::value::ErrorObj;
    let tag: Arc<str> = match err {
        EvalError::UnboundSymbol { .. } => Arc::from("unbound-symbol"),
        EvalError::ArityMismatch { .. } => Arc::from("arity-mismatch"),
        EvalError::TypeMismatch { .. } => Arc::from("type-mismatch"),
        EvalError::DivisionByZero { .. } => Arc::from("division-by-zero"),
        EvalError::NotCallable { .. } => Arc::from("not-callable"),
        EvalError::BadSpecialForm { .. } => Arc::from("bad-special-form"),
        EvalError::NativeFn { .. } => Arc::from("native-fn"),
        EvalError::Reader(_) => Arc::from("reader"),
        EvalError::Halted => Arc::from("halted"),
        EvalError::NotImplemented(_) => Arc::from("not-implemented"),
        EvalError::User { .. } => Arc::from("user"),
    };
    let message: Arc<str> = Arc::from(err.short_message());
    Value::Error(Arc::new(ErrorObj {
        tag,
        message,
        data: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitive::install_primitives;
    use tatara_lisp::read_spanned;

    struct NoHost;

    fn eval_ok(src: &str) -> Value {
        let forms = read_spanned(src).unwrap();
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        i.eval_program(&forms, &mut host).unwrap()
    }

    fn eval_err(src: &str) -> EvalError {
        let forms = read_spanned(src).unwrap();
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        i.eval_program(&forms, &mut host).unwrap_err()
    }

    // ── Literals + symbol lookup ──────────────────────────────────

    #[test]
    fn literal_int() {
        assert!(matches!(eval_ok("42"), Value::Int(42)));
    }

    #[test]
    fn unbound_symbol_errors() {
        let e = eval_err("no-such-var");
        assert!(matches!(e, EvalError::UnboundSymbol { .. }));
    }

    #[test]
    fn quote_returns_runtime_list_of_symbols() {
        // Scheme/Clojure semantics: '(a b c) yields a runtime list of
        // three symbols, not a wrapped source-form Sexp.
        let v = eval_ok("'(a b c)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[0], Value::Symbol(s) if s.as_ref() == "a"));
                assert!(matches!(&xs[1], Value::Symbol(s) if s.as_ref() == "b"));
                assert!(matches!(&xs[2], Value::Symbol(s) if s.as_ref() == "c"));
            }
            other => panic!("{other:?}"),
        }
    }

    // ── Arithmetic via primitives ─────────────────────────────────

    #[test]
    fn add_ints() {
        assert!(matches!(eval_ok("(+ 1 2 3)"), Value::Int(6)));
    }

    #[test]
    fn sub_divides_float() {
        match eval_ok("(- 10 3)") {
            Value::Int(7) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn division_by_zero_errors() {
        assert!(matches!(
            eval_err("(/ 1 0)"),
            EvalError::DivisionByZero { .. }
        ));
    }

    // ── Conditionals ──────────────────────────────────────────────

    #[test]
    fn if_truthy_branch() {
        assert!(matches!(eval_ok("(if #t 1 2)"), Value::Int(1)));
    }

    #[test]
    fn if_falsy_branch() {
        assert!(matches!(eval_ok("(if #f 1 2)"), Value::Int(2)));
    }

    #[test]
    fn if_no_else_returns_nil() {
        assert!(matches!(eval_ok("(if #f 1)"), Value::Nil));
    }

    #[test]
    fn cond_picks_first_match() {
        assert!(matches!(
            eval_ok("(cond (#f 1) (#t 2) (else 3))"),
            Value::Int(2)
        ));
    }

    #[test]
    fn cond_falls_through_to_else() {
        assert!(matches!(
            eval_ok("(cond (#f 1) (#f 2) (else 3))"),
            Value::Int(3)
        ));
    }

    #[test]
    fn when_runs_body_if_true() {
        assert!(matches!(eval_ok("(when #t 99)"), Value::Int(99)));
        assert!(matches!(eval_ok("(when #f 99)"), Value::Nil));
    }

    // ── Let forms ─────────────────────────────────────────────────

    #[test]
    fn let_binds_and_evaluates_body() {
        assert!(matches!(
            eval_ok("(let ((x 10) (y 20)) (+ x y))"),
            Value::Int(30)
        ));
    }

    #[test]
    fn let_star_sequential_bindings() {
        assert!(matches!(
            eval_ok("(let* ((x 5) (y (+ x 1))) (+ x y))"),
            Value::Int(11)
        ));
    }

    #[test]
    fn letrec_mutual_recursion() {
        let v = eval_ok(
            "(letrec ((even? (lambda (n) (if (= n 0) #t (odd? (- n 1)))))
                      (odd?  (lambda (n) (if (= n 0) #f (even? (- n 1))))))
               (even? 10))",
        );
        assert!(matches!(v, Value::Bool(true)));
    }

    // ── Lambda + closure ──────────────────────────────────────────

    #[test]
    fn lambda_applies() {
        assert!(matches!(
            eval_ok("((lambda (x y) (+ x y)) 3 4)"),
            Value::Int(7)
        ));
    }

    #[test]
    fn lambda_closes_over_env() {
        assert!(matches!(
            eval_ok("(let ((n 10)) ((lambda (x) (+ x n)) 5))"),
            Value::Int(15)
        ));
    }

    #[test]
    fn closure_captures_by_value_at_creation() {
        // make-adder style — the returned closure should capture n=5 even
        // though the outer let scope has exited.
        let v = eval_ok(
            "(define make-adder (lambda (n) (lambda (x) (+ x n))))
             (define add5 (make-adder 5))
             (add5 10)",
        );
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn rest_args_collect_into_list() {
        let v = eval_ok("((lambda (x &rest rs) (length rs)) 1 2 3 4 5)");
        assert!(matches!(v, Value::Int(4)));
    }

    #[test]
    fn closure_arity_mismatch() {
        let e = eval_err("((lambda (x y) (+ x y)) 1)");
        assert!(matches!(e, EvalError::ArityMismatch { .. }));
    }

    // ── Define + set! ─────────────────────────────────────────────

    #[test]
    fn define_then_use() {
        assert!(matches!(eval_ok("(define x 42) x"), Value::Int(42)));
    }

    #[test]
    fn define_function_shorthand() {
        assert!(matches!(
            eval_ok("(define (sq x) (* x x)) (sq 6)"),
            Value::Int(36)
        ));
    }

    #[test]
    fn set_mutates_existing() {
        assert!(matches!(
            eval_ok("(define x 1) (set! x 99) x"),
            Value::Int(99)
        ));
    }

    #[test]
    fn set_unbound_errors() {
        let e = eval_err("(set! nope 1)");
        assert!(matches!(e, EvalError::UnboundSymbol { .. }));
    }

    // ── begin / and / or / not ────────────────────────────────────

    #[test]
    fn begin_returns_last() {
        assert!(matches!(eval_ok("(begin 1 2 3)"), Value::Int(3)));
    }

    #[test]
    fn and_short_circuits() {
        assert!(matches!(eval_ok("(and 1 #f 2)"), Value::Bool(false)));
        assert!(matches!(eval_ok("(and 1 2 3)"), Value::Int(3)));
        assert!(matches!(eval_ok("(and)"), Value::Bool(true)));
    }

    #[test]
    fn or_short_circuits() {
        assert!(matches!(eval_ok("(or #f #f 7)"), Value::Int(7)));
        assert!(matches!(eval_ok("(or #f #f)"), Value::Bool(false)));
        assert!(matches!(eval_ok("(or)"), Value::Bool(false)));
    }

    #[test]
    fn not_inverts() {
        assert!(matches!(eval_ok("(not #t)"), Value::Bool(false)));
        assert!(matches!(eval_ok("(not #f)"), Value::Bool(true)));
        assert!(matches!(eval_ok("(not 42)"), Value::Bool(false)));
    }

    // ── Recursion ─────────────────────────────────────────────────

    #[test]
    fn recursive_factorial() {
        let v = eval_ok(
            "(define (fact n)
               (if (= n 0) 1 (* n (fact (- n 1)))))
             (fact 6)",
        );
        assert!(matches!(v, Value::Int(720)));
    }

    #[test]
    fn recursive_length() {
        let v = eval_ok(
            "(define (len xs)
               (if (null? xs) 0 (+ 1 (len (cdr xs)))))
             (len (list 1 2 3 4 5))",
        );
        assert!(matches!(v, Value::Int(5)));
    }

    // ── Host context reachable via register_fn ────────────────────

    // ── Quasiquote ────────────────────────────────────────────────

    #[test]
    fn quasiquote_plain_list_is_runtime_list() {
        let v = eval_ok("`(a b c)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[0], Value::Symbol(s) if s.as_ref() == "a"));
                assert!(matches!(&xs[1], Value::Symbol(s) if s.as_ref() == "b"));
                assert!(matches!(&xs[2], Value::Symbol(s) if s.as_ref() == "c"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_unquote_substitutes_evaluated_value() {
        let v = eval_ok("(let ((x 42)) `(a ,x c))");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                assert!(matches!(&xs[1], Value::Int(42)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_unquote_arbitrary_expr() {
        let v = eval_ok("`(x ,(+ 1 2 3) y)");
        match v {
            Value::List(xs) => {
                assert!(matches!(&xs[1], Value::Int(6)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_splice_inlines_list() {
        let v = eval_ok("`(a ,@(list 1 2 3) b)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 5);
                assert!(matches!(&xs[0], Value::Symbol(s) if s.as_ref() == "a"));
                assert!(matches!(&xs[1], Value::Int(1)));
                assert!(matches!(&xs[2], Value::Int(2)));
                assert!(matches!(&xs[3], Value::Int(3)));
                assert!(matches!(&xs[4], Value::Symbol(s) if s.as_ref() == "b"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_splice_empty_list_splices_nothing() {
        let v = eval_ok("`(a ,@(list) b)");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 2);
                assert!(matches!(&xs[0], Value::Symbol(s) if s.as_ref() == "a"));
                assert!(matches!(&xs[1], Value::Symbol(s) if s.as_ref() == "b"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_splice_non_list_errors() {
        let e = eval_err("`(a ,@42)");
        assert!(matches!(e, EvalError::TypeMismatch { .. }));
    }

    #[test]
    fn quasiquote_atom_yields_atom_value() {
        assert!(matches!(eval_ok("`foo"), Value::Symbol(s) if s.as_ref() == "foo"));
        assert!(matches!(eval_ok("`42"), Value::Int(42)));
    }

    #[test]
    fn quasiquote_with_nested_list_and_unquote() {
        // `(foo (bar ,x) baz) where x=99 → (foo (bar 99) baz)
        let v = eval_ok("(let ((x 99)) `(foo (bar ,x) baz))");
        match v {
            Value::List(xs) => {
                assert_eq!(xs.len(), 3);
                match &xs[1] {
                    Value::List(inner) => {
                        assert!(matches!(&inner[1], Value::Int(99)));
                    }
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quasiquote_symbol_keyword_distinction_preserved() {
        let v = eval_ok("`(:key val)");
        match v {
            Value::List(xs) => {
                assert!(matches!(&xs[0], Value::Keyword(s) if s.as_ref() == "key"));
                assert!(matches!(&xs[1], Value::Symbol(s) if s.as_ref() == "val"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bare_unquote_outside_quasiquote_errors() {
        let e = eval_err(",x");
        assert!(matches!(e, EvalError::BadSpecialForm { .. }));
    }

    // ── Host context reachable via register_fn ────────────────────

    #[test]
    fn native_fn_reads_host_state() {
        struct Counter {
            n: i64,
        }
        let forms = read_spanned("(bump) (bump) (bump) (cur)").unwrap();
        let mut i: Interpreter<Counter> = Interpreter::new();
        install_primitives(&mut i);
        i.register_fn(
            "bump",
            Arity::Exact(0),
            |_args: &[Value], host: &mut Counter, _span| {
                host.n += 1;
                Ok(Value::Int(host.n))
            },
        );
        i.register_fn(
            "cur",
            Arity::Exact(0),
            |_args: &[Value], host: &mut Counter, _span| Ok(Value::Int(host.n)),
        );
        let mut host = Counter { n: 0 };
        let v = i.eval_program(&forms, &mut host).unwrap();
        assert!(matches!(v, Value::Int(3)));
    }

    // ── Typed FFI registration ────────────────────────────────────

    struct Ctx {
        records: Vec<(String, i64)>,
    }

    #[test]
    fn register_typed1_marshals_string_arg() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed1("greet", |_h: &mut Ctx, name: String| -> Result<String> {
            Ok(format!("hello {name}"))
        });
        let forms = read_spanned(r#"(greet "luis")"#).unwrap();
        let mut h = Ctx { records: vec![] };
        let v = i.eval_program(&forms, &mut h).unwrap();
        match v {
            Value::Str(s) => assert_eq!(&*s, "hello luis"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn register_typed2_marshals_host_state_mutation() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed2(
            "record",
            |h: &mut Ctx, name: String, n: i64| -> Result<()> {
                h.records.push((name, n));
                Ok(())
            },
        );
        let forms = read_spanned(r#"(record "a" 1) (record "b" 2)"#).unwrap();
        let mut h = Ctx { records: vec![] };
        let _ = i.eval_program(&forms, &mut h).unwrap();
        assert_eq!(h.records.len(), 2);
        assert_eq!(h.records[0], ("a".to_string(), 1));
        assert_eq!(h.records[1], ("b".to_string(), 2));
    }

    #[test]
    fn register_typed_arg_type_mismatch_surfaces_at_call_site() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed1("needs-int", |_h: &mut Ctx, n: i64| -> Result<i64> {
            Ok(n + 1)
        });
        let forms = read_spanned(r#"(needs-int "not-a-number")"#).unwrap();
        let mut h = Ctx { records: vec![] };
        let err = i.eval_program(&forms, &mut h).unwrap_err();
        assert!(matches!(
            err,
            EvalError::TypeMismatch {
                expected: "integer",
                ..
            }
        ));
    }

    #[test]
    fn register_typed3_three_args() {
        let mut i: Interpreter<Ctx> = Interpreter::new();
        install_primitives(&mut i);
        i.register_typed3(
            "triple-sum",
            |_h: &mut Ctx, a: i64, b: i64, c: i64| -> Result<i64> { Ok(a + b + c) },
        );
        let forms = read_spanned("(triple-sum 10 20 30)").unwrap();
        let mut h = Ctx { records: vec![] };
        let v = i.eval_program(&forms, &mut h).unwrap();
        assert!(matches!(v, Value::Int(60)));
    }

    // ── User macros via defmacro ──────────────────────────────────

    #[test]
    fn user_macro_expands_and_evaluates() {
        let v = eval_ok(
            "(defmacro twice (x) `(* ,x 2))
             (twice 21)",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn user_macro_definition_returns_nil() {
        let v = eval_ok("(defmacro inc (x) `(+ ,x 1))");
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn user_macro_inside_define_body_expands() {
        // (define (f n) (inc n)) — the (inc n) call is rewritten to (+ n 1)
        // before define captures the body.
        let v = eval_ok(
            "(defmacro inc (x) `(+ ,x 1))
             (define (f n) (inc n))
             (f 41)",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn user_macro_with_rest_args_splices() {
        let v = eval_ok(
            "(defmacro sum-all (&rest xs) `(+ ,@xs))
             (sum-all 1 2 3 4 5)",
        );
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn nested_user_macros_compose() {
        let v = eval_ok(
            "(defmacro twice (x) `(* ,x 2))
             (defmacro quad (x) `(twice (twice ,x)))
             (quad 5)",
        );
        assert!(matches!(v, Value::Int(20)));
    }

    #[test]
    fn user_macro_can_expand_to_special_form() {
        // Macros can expand into special forms — `if`, `let`, `lambda`,
        // `define` are all reachable as expansion targets.
        let v = eval_ok(
            "(defmacro guard (test then) `(if ,test ,then 0))
             (guard #t 99)",
        );
        assert!(matches!(v, Value::Int(99)));
    }

    #[test]
    fn user_macro_redefined_replaces_prior_template() {
        let v = eval_ok(
            "(defmacro k () `1)
             (defmacro k () `2)
             (k)",
        );
        assert!(matches!(v, Value::Int(2)));
    }

    #[test]
    fn user_macro_unbound_template_var_errors() {
        // ,y refers to a name not bound in the macro's parameter list
        // and not defined in the surrounding scope. Under the
        // full-eval expander this surfaces as a proper unbound-symbol
        // error at expansion time, with the offending symbol in the
        // payload — strictly better than the legacy "compile" error.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let forms = read_spanned("(defmacro bad (x) `(list ,y)) (bad 1)").unwrap();
        let err = i.eval_program(&forms, &mut NoHost).unwrap_err();
        match err {
            EvalError::UnboundSymbol { name, .. } => assert_eq!(&*name, "y"),
            other => panic!("expected UnboundSymbol, got {other:?}"),
        }
    }

    #[test]
    fn defpoint_template_keyword_registers_as_macro() {
        // `defpoint-template` is the typed-DSL spelling of `defmacro` —
        // the runtime should accept both.
        let v = eval_ok(
            "(defpoint-template double (x) `(* ,x 2))
             (double 7)",
        );
        assert!(matches!(v, Value::Int(14)));
    }

    #[test]
    fn defcheck_keyword_registers_as_macro() {
        let v = eval_ok(
            "(defcheck always-7 () `7)
             (always-7)",
        );
        assert!(matches!(v, Value::Int(7)));
    }

    #[test]
    fn macro_call_evaluated_with_runtime_arg() {
        // Macro arg is itself an expression — the substituted expression
        // is evaluated *after* expansion, so the arg's runtime value is
        // what reaches the expanded form.
        let v = eval_ok(
            "(defmacro double (x) `(+ ,x ,x))
             (define n 13)
             (double n)",
        );
        assert!(matches!(v, Value::Int(26)));
    }

    #[test]
    fn macro_persists_across_eval_program_calls() {
        // The expander state outlives a single eval_program call — REPL
        // semantics rely on this.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        let defs = read_spanned("(defmacro inc (x) `(+ ,x 1))").unwrap();
        i.eval_program(&defs, &mut host).unwrap();
        assert_eq!(i.expander().len(), 1);

        let call = read_spanned("(inc 41)").unwrap();
        let v = i.eval_program(&call, &mut host).unwrap();
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn macro_expansion_inside_lambda_body() {
        let v = eval_ok(
            "(defmacro sq (x) `(* ,x ,x))
             ((lambda (n) (sq n)) 9)",
        );
        assert!(matches!(v, Value::Int(81)));
    }

    #[test]
    fn no_macros_registered_keeps_eval_program_a_passthrough() {
        // Sanity: with no macros registered, eval_program should still run
        // every existing test path correctly. Touching the same code as
        // the rest of the suite — this just asserts the optimization
        // we baked in (skip expand when expander is empty) didn't
        // accidentally drop forms.
        let v = eval_ok("(+ 1 2 3)");
        assert!(matches!(v, Value::Int(6)));
    }

    #[test]
    fn eval_top_form_drives_one_form_at_a_time() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_primitives(&mut i);
        let mut host = NoHost;
        let forms = read_spanned("(defmacro id (x) `,x) (id 42)").unwrap();

        // First form: registers, returns Nil.
        let r0 = i.eval_top_form(&forms[0], &mut host).unwrap();
        assert!(matches!(r0, Value::Nil));

        // Second form: macro expanded → 42.
        let r1 = i.eval_top_form(&forms[1], &mut host).unwrap();
        assert!(matches!(r1, Value::Int(42)));
    }

    // ── Full-eval macroexpansion power tests ──────────────────────
    //
    // These exercise the Racket/CL/Clojure-grade macro model: the
    // macro body is a regular Lisp program evaluated at expansion time
    // with full access to every primitive and library fn.

    use crate::install_full_stdlib_with;

    fn run_full(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    #[test]
    fn macro_can_use_map_at_expansion_time() {
        // The macro body uses (map ...) at expansion time to transform
        // each arg into a different form. Result: a `(list ...)` whose
        // children are the squared symbols' representations.
        let v = run_full(
            "(defmacro double-each (&rest xs)
               `(list ,@(map (lambda (x) (* x 2)) xs)))
             (double-each 1 2 3 4 5)",
        );
        assert_eq!(format!("{v}"), "(2 4 6 8 10)");
    }

    #[test]
    fn macro_can_use_foldl_at_expansion_time() {
        // The expansion ITSELF is built by folding — the macro returns
        // a sum-of-args expression, but only after expansion-time
        // computation chooses the additive form.
        let v = run_full(
            "(defmacro static-sum (&rest xs)
               (foldl + 0 xs))
             (static-sum 1 2 3 4 5)",
        );
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn macro_can_use_filter_at_expansion_time() {
        // Macro args arrive as source-form Values: literals stay
        // literals, but `(- 4)` is a List not a negative number.
        // Use direct negative literals so the filter sees integers.
        let v = run_full(
            "(defmacro sum-positives (&rest xs)
               `(+ ,@(filter positive? xs)))
             (sum-positives 1 -2 3 -4 5)",
        );
        // Filter to (1 3 5) at expansion → emit (+ 1 3 5) → 9.
        assert!(matches!(v, Value::Int(9)));
    }

    #[test]
    fn macro_can_recursively_emit_let_chain() {
        // (chain-let (a 1) (b 2) (c 3) body) →
        //   (let ((a 1)) (let ((b 2)) (let ((c 3)) body))).
        let v = run_full(
            "(defmacro chain-let (binding &rest more)
               (if (null? more)
                   `(let (,binding) #t)
                   `(let (,binding) (chain-let ,@more))))
             (chain-let (a 1) (b 2) (c 3))",
        );
        assert!(matches!(v, Value::Bool(true)));
    }

    #[test]
    fn macro_can_use_gensym_for_hygiene() {
        // The macro introduces a fresh local binding via gensym, so
        // no name collision risk.
        let v = run_full(
            "(defmacro swap-bind (init body)
               (let ((tmp (gensym \"tmp\")))
                 `(let ((,tmp ,init))
                    (+ ,tmp ,tmp))))
             (swap-bind 21 #t)",
        );
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn macro_can_inspect_arg_shape() {
        // Detect whether the arg is a list and emit different code.
        let v = run_full(
            "(defmacro shape-aware (x)
               (if (list? x)
                   `(+ ,@x)         ;; sum the children
                   `,x))            ;; pass through scalars
             (+ (shape-aware (1 2 3)) (shape-aware 100))",
        );
        // (1 2 3) → 6; 100 → 100; total → 106.
        assert!(matches!(v, Value::Int(106)));
    }

    #[test]
    fn macro_can_call_user_helper_fn() {
        // Define a helper at top level; macro body calls it at expand.
        let v = run_full(
            "(define (square x) (* x x))
             (defmacro static-square (n) (square n))
             (static-square 7)",
        );
        assert!(matches!(v, Value::Int(49)));
    }

    #[test]
    fn macro_emitting_quoted_form_round_trips() {
        // A macro that produces a quoted constant — the (quote x)
        // representation must round-trip cleanly.
        let v = run_full(
            "(defmacro literal-list (&rest xs)
               `(quote ,xs))
             (literal-list a b c)",
        );
        let s = format!("{v}");
        assert!(s.contains('a') && s.contains('b') && s.contains('c'));
    }

    #[test]
    fn quasiquote_inside_quasiquote_in_macro_output_is_preserved() {
        // A macro that emits a quasiquote at runtime — the runtime
        // should see a quasiquote and evaluate it.
        let v = run_full(
            "(defmacro emit-qq (x) `(quasiquote (a (unquote ,x) c)))
             (let ((q (emit-qq 99))) q)",
        );
        // Result is the runtime-value (a 99 c).
        assert_eq!(format!("{v}"), "(a 99 c)");
    }

    #[test]
    fn macro_body_can_define_locals_and_dispatch() {
        // Macro body uses let + cond + map — full programmability.
        let v = run_full(
            "(defmacro classify-args (&rest xs)
               (let ((evens (filter even? xs))
                     (odds  (filter odd?  xs)))
                 `(list (list :evens ,@evens)
                        (list :odds  ,@odds))))
             (classify-args 1 2 3 4 5 6)",
        );
        let s = format!("{v}");
        assert!(s.contains(":evens 2 4 6"));
        assert!(s.contains(":odds 1 3 5"));
    }

    // ── Tail-call optimization tests ──────────────────────────────
    //
    // These prove the trampoline catches the standard tail positions:
    // direct self-recursion through `if`, mutual recursion, deep
    // recursion through `cond`, `let`-body, and `begin`. Without TCO,
    // each would stack-overflow at ~10k frames; with TCO they run in
    // bounded space.

    #[test]
    fn tco_self_recursion_via_if() {
        // Sum integers 1..n via accumulator. Tail call inside `if` else
        // branch. n=100_000 would overflow the default Rust stack
        // without TCO.
        let v = run_full(
            "(define (sum n acc)
               (if (= n 0)
                   acc
                   (sum (- n 1) (+ acc n))))
             (sum 100000 0)",
        );
        // n*(n+1)/2 = 5_000_050_000
        assert!(matches!(v, Value::Int(5_000_050_000)));
    }

    #[test]
    fn tco_mutual_recursion() {
        // Two closures call each other in tail position. Trampoline
        // must support the closure swap.
        let v = run_full(
            "(define (even-r? n) (if (= n 0) #t (odd-r? (- n 1))))
             (define (odd-r?  n) (if (= n 0) #f (even-r? (- n 1))))
             (even-r? 50000)",
        );
        assert!(matches!(v, Value::Bool(true)));
    }

    #[test]
    fn tco_via_cond_branch() {
        let v = run_full(
            "(define (countdown n)
               (cond
                 ((<= n 0) :done)
                 (else (countdown (- n 1)))))
             (countdown 50000)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "done"));
    }

    #[test]
    fn tco_via_let_body() {
        // Tail call inside the BODY of a `let`. Trampoline must respect
        // that the let frame is on env when entering the call.
        let v = run_full(
            "(define (loop-let n)
               (let ((m (- n 1)))
                 (if (<= n 0) :done (loop-let m))))
             (loop-let 50000)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "done"));
    }

    #[test]
    fn tco_via_begin_last_form() {
        let v = run_full(
            "(define (counter n)
               (begin
                 (+ 1 1)
                 (+ 2 2)
                 (if (<= n 0) :done (counter (- n 1)))))
             (counter 50000)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "done"));
    }

    #[test]
    fn tco_via_when_unless() {
        let v = run_full(
            "(define (drain n)
               (when (> n 0)
                 (drain (- n 1))))
             (drain 50000)",
        );
        // when's else branch returns nil; here recurses inside.
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn tco_through_and_or_short_circuit_last() {
        // `and` returns the last value if all are truthy. The last form
        // is in tail position.
        let v = run_full(
            "(define (loop-and n)
               (and #t #t (if (<= n 0) :done (loop-and (- n 1)))))
             (loop-and 30000)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "done"));
    }

    #[test]
    fn non_tail_recursion_still_works_for_small_n() {
        // Non-tail recursion: (* n (fact (- n 1))) — the multiply
        // happens AFTER the recursive call returns, so it's not a tail
        // call. Should still work for moderate n via the regular stack.
        let v = run_full(
            "(define (fact n)
               (if (= n 0) 1 (* n (fact (- n 1)))))
             (fact 12)",
        );
        // 12! = 479_001_600
        assert!(matches!(v, Value::Int(479_001_600)));
    }

    // ── Structured errors / try / catch ────────────────────────────

    #[test]
    fn error_constructor_returns_error_value() {
        let v = run_full("(error :validation \"bad input\")");
        match v {
            Value::Error(e) => {
                assert_eq!(&*e.tag, "validation");
                assert_eq!(&*e.message, "bad input");
                assert!(e.data.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ex_info_uses_default_tag() {
        let v = run_full("(ex-info \"validation failed\" (list :field \"email\" :code 42))");
        match v {
            Value::Error(e) => {
                assert_eq!(&*e.tag, "ex-info");
                assert_eq!(&*e.message, "validation failed");
                assert_eq!(e.data.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn error_predicate() {
        let v = run_full("(error? (error :x \"y\"))");
        assert!(matches!(v, Value::Bool(true)));
        let v = run_full("(error? 42)");
        assert!(matches!(v, Value::Bool(false)));
    }

    #[test]
    fn error_accessors() {
        let v = run_full(
            "(let ((e (ex-info \"oops\" (list :user-id 42))))
               (list (error-tag e) (error-message e) (error-data-get e :user-id)))",
        );
        assert_eq!(format!("{v}"), "(:ex-info \"oops\" 42)");
    }

    #[test]
    fn try_catches_thrown_error() {
        let v = run_full(
            "(try
               (throw (ex-info \"boom\" (list :code 500)))
               (catch (e)
                 (error-message e)))",
        );
        assert_eq!(format!("{v}"), "\"boom\"");
    }

    #[test]
    fn try_returns_body_value_when_no_throw() {
        let v = run_full(
            "(try
               (+ 1 2 3)
               (catch (e) :unreachable))",
        );
        assert!(matches!(v, Value::Int(6)));
    }

    #[test]
    fn try_catches_runtime_errors_too() {
        // Division by zero is a Rust-side EvalError, not a user throw.
        // The catch handler should still observe it (wrapped to
        // Value::Error with tag :division-by-zero).
        let v = run_full(
            "(try
               (/ 1 0)
               (catch (e) (error-tag e)))",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "division-by-zero"));
    }

    #[test]
    fn try_catches_unbound_symbol_error() {
        let v = run_full(
            "(try
               undefined-var
               (catch (e) (error-tag e)))",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "unbound-symbol"));
    }

    #[test]
    fn try_catches_arity_mismatch() {
        let v = run_full(
            "(try
               ((lambda (x y) (+ x y)) 1)
               (catch (e) (error-tag e)))",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "arity-mismatch"));
    }

    #[test]
    fn nested_try_inner_handler_takes_precedence() {
        let v = run_full(
            "(try
               (try
                 (throw (ex-info \"inner\" ()))
                 (catch (e) :inner-caught))
               (catch (e) :outer-caught))",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "inner-caught"));
    }

    #[test]
    fn outer_try_catches_when_handler_rethrows() {
        let v = run_full(
            "(try
               (try
                 (throw (ex-info \"first\" ()))
                 (catch (e) (throw (ex-info \"rethrown\" ()))))
               (catch (e) (error-message e)))",
        );
        assert_eq!(format!("{v}"), "\"rethrown\"");
    }

    #[test]
    fn throw_propagates_when_no_try() {
        // Without try, throw bubbles up as EvalError::User.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let forms = read_spanned("(throw (ex-info \"unhandled\" (list :code 99)))").unwrap();
        let err = i.eval_program(&forms, &mut NoHost).unwrap_err();
        match err {
            EvalError::User { value, .. } => match value {
                Value::Error(e) => {
                    assert_eq!(&*e.message, "unhandled");
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    // ── macroexpand-1 / macroexpand introspection ─────────────────

    #[test]
    fn macroexpand_one_step() {
        let v = run_full(
            "(defmacro twice (x) `(* ,x 2))
             (macroexpand-1 '(twice 7))",
        );
        // Single step: (twice 7) → (* 7 2)
        assert_eq!(format!("{v}"), "(* 7 2)");
    }

    #[test]
    fn macroexpand_full_until_fixed_point() {
        let v = run_full(
            "(defmacro twice (x) `(* ,x 2))
             (defmacro quad (x) `(twice (twice ,x)))
             (macroexpand '(quad 5))",
        );
        // (quad 5) → (twice (twice 5)) → (twice (* 5 2)) → (* (* 5 2) 2)
        assert_eq!(format!("{v}"), "(* (* 5 2) 2)");
    }

    #[test]
    fn macroexpand_returns_unchanged_for_non_macro() {
        let v = run_full("(macroexpand-1 '(+ 1 2 3))");
        // + isn't a macro — passes through.
        assert_eq!(format!("{v}"), "(+ 1 2 3)");
    }

    #[test]
    fn macroexpand_one_does_not_recurse_into_children() {
        // Only the head is expanded one level. Inner macro calls remain.
        let v = run_full(
            "(defmacro twice (x) `(* ,x 2))
             (defmacro outer (x) `(list ,x))
             (macroexpand-1 '(outer (twice 3)))",
        );
        // (outer (twice 3)) → (list (twice 3))   — inner macro NOT expanded.
        assert_eq!(format!("{v}"), "(list (twice 3))");
    }

    #[test]
    fn macroexpand_recurses_into_children() {
        let v = run_full(
            "(defmacro twice (x) `(* ,x 2))
             (defmacro outer (x) `(list ,x))
             (macroexpand '(outer (twice 3)))",
        );
        // Full expansion expands inner: (list (* 3 2))
        assert_eq!(format!("{v}"), "(list (* 3 2))");
    }

    // ── Module system: provide / require / qualified names ────────

    fn run_with_modules(modules: &[(&str, &str)], src: &str) -> Value {
        use crate::module::MapLoader;
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let mut loader = MapLoader::new();
        for (path, source) in modules {
            loader.insert(*path, *source);
        }
        i.set_loader(Arc::new(loader));
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    fn run_with_modules_err(modules: &[(&str, &str)], src: &str) -> EvalError {
        use crate::module::MapLoader;
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let mut loader = MapLoader::new();
        for (path, source) in modules {
            loader.insert(*path, *source);
        }
        i.set_loader(Arc::new(loader));
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap_err()
    }

    #[test]
    fn require_with_explicit_alias_imports_qualified_names() {
        let v = run_with_modules(
            &[(
                "lib/math",
                "(define square (lambda (x) (* x x)))
                 (define cube (lambda (x) (* x x x)))
                 (provide square cube)",
            )],
            "(require \"lib/math\" :as math)
             (math/square 7)",
        );
        assert!(matches!(v, Value::Int(49)));
    }

    #[test]
    fn require_uses_path_as_default_alias() {
        let v = run_with_modules(
            &[("lib/math", "(define double (lambda (x) (* x 2))) (provide double)")],
            "(require \"lib/math\")
             (lib/math/double 21)",
        );
        // No explicit :as alias → bound under the path itself, so
        // `lib/math/double` is the qualified name.
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn require_refer_imports_unqualified_names() {
        let v = run_with_modules(
            &[(
                "lib/math",
                "(define square (lambda (x) (* x x)))
                 (define cube (lambda (x) (* x x x)))
                 (provide square cube)",
            )],
            "(require \"lib/math\" :refer (square))
             (square 6)",
        );
        assert!(matches!(v, Value::Int(36)));
    }

    #[test]
    fn require_does_not_import_non_provided() {
        // `private` is defined but NOT provided — should not be
        // accessible from the importing module.
        let err = run_with_modules_err(
            &[(
                "lib/secret",
                "(define public 1)
                 (define private 2)
                 (provide public)",
            )],
            "(require \"lib/secret\" :as s)
             s/private",
        );
        match err {
            EvalError::UnboundSymbol { name, .. } => assert_eq!(&*name, "s/private"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn require_chain_a_imports_b() {
        let v = run_with_modules(
            &[
                (
                    "lib/util",
                    "(define inc1 (lambda (n) (+ n 1)))
                     (provide inc1)",
                ),
                (
                    "lib/wrapper",
                    "(require \"lib/util\" :as u)
                     (define inc2 (lambda (n) (u/inc1 (u/inc1 n))))
                     (provide inc2)",
                ),
            ],
            "(require \"lib/wrapper\" :as w)
             (w/inc2 10)",
        );
        assert!(matches!(v, Value::Int(12)));
    }

    #[test]
    fn require_module_not_found() {
        let err = run_with_modules_err(&[], "(require \"missing/module\")");
        // Surfaces as a Value::Error inside EvalError::User.
        match err {
            EvalError::User { value, .. } => match value {
                Value::Error(e) => {
                    assert_eq!(&*e.tag, "module-not-found");
                    assert!(e.message.contains("missing/module"));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn circular_require_detected() {
        let err = run_with_modules_err(
            &[
                ("a", "(require \"b\") (provide x) (define x 1)"),
                ("b", "(require \"a\") (provide y) (define y 2)"),
            ],
            "(require \"a\")",
        );
        match err {
            EvalError::User { value, .. } => match value {
                Value::Error(e) => assert_eq!(&*e.tag, "circular-require"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn provide_at_top_level_errors() {
        // Without being inside a require, (provide ...) is meaningless.
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let forms = read_spanned("(provide x)").unwrap();
        let err = i.eval_program(&forms, &mut NoHost).unwrap_err();
        assert!(matches!(err, EvalError::BadSpecialForm { form, .. } if &*form == "provide"));
    }

    #[test]
    fn require_refer_unknown_name_errors() {
        let err = run_with_modules_err(
            &[(
                "lib/math",
                "(define square (lambda (x) (* x x))) (provide square)",
            )],
            "(require \"lib/math\" :refer (square cube))",
        );
        match err {
            EvalError::User { value, .. } => match value {
                Value::Error(e) => {
                    assert!(matches!(&*e.tag, "not-defined" | "not-exported"));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn require_caches_module_load_once() {
        let v = run_with_modules(
            &[(
                "lib/foo",
                "(define x 42) (provide x)",
            )],
            "(require \"lib/foo\" :as a)
             (require \"lib/foo\" :as b)
             (+ a/x b/x)",
        );
        // Both alias to the same cached module.
        assert!(matches!(v, Value::Int(84)));
    }
}
