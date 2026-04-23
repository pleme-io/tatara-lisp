# tatara-lisp-eval — Design

Runtime evaluator for tatara-lisp. Sibling crate to `tatara-lisp` (reader +
macroexpander + typed-domain compiler). Enables runtime `eval` of already-read
`Sexp` forms against an embedder-controlled host context.

First consumer: `hanshi` (pleme-io UV printer orchestrator) — live orchestration
DSL + hot-reloaded rule bundles + remote REPL over Tailscale.

## Goal

A small, embeddable, sandboxable Scheme-ish evaluator scoped to
**orchestration**, not general-purpose computing. Just enough to express
job queues, state-machine transitions, rule/advisor logic, and ad-hoc recon
queries against a Rust host.

## Non-goals

- **Not** a full R7RS implementation. No continuations, no `call/cc`, no
  full numeric tower (rationals, complex, exact/inexact distinction), no
  `dynamic-wind`.
- **Not** a performance story. Target: tens of thousands of evals/s, not
  millions. Correctness and clarity over throughput.
- **Not** a replacement for tatara-lisp's typed-domain compiler. That remains
  the committed / declarative / cacheable path. `eval` is for the runtime /
  ephemeral / REPL path.
- **Not** a macro system of its own. Macros are tatara-lisp's concern; by the
  time a form reaches the evaluator, it has been expanded.

## Crate layout

```
tatara-lisp/                        (existing workspace)
├── tatara-lisp/                    (existing — reader, expander, compiler)
├── tatara-lisp-derive/             (existing — proc macros)
└── tatara-lisp-eval/               (new)
    ├── Cargo.toml
    └── src/
        ├── lib.rs                  (public surface)
        ├── value.rs                (Value type — runtime values)
        ├── eval.rs                 (core evaluator)
        ├── env.rs                  (eval-side environment — holds Values)
        ├── special.rs              (special-form dispatch)
        ├── primitive.rs            (built-in procedures)
        ├── ffi.rs                  (register_fn, host context)
        ├── repl.rs                 (streaming form eval)
        └── error.rs                (runtime errors with source locations)
```

`tatara-lisp-eval` depends on `tatara-lisp` for `Sexp`, `Atom`, `reader::read`,
`Expander`, `LispError`. It does **not** modify tatara-lisp — strict
forward-compatible extension.

## Value model

The evaluator distinguishes between **source forms** (`Sexp`, produced by the
reader) and **runtime values** (`Value`, produced by evaluation).

```rust
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    Symbol(Arc<str>),
    Keyword(Arc<str>),
    List(Arc<Vec<Value>>),
    Closure(Arc<Closure>),
    NativeFn(Arc<NativeFn>),
    Sexp(Sexp),                 // escape hatch: unevaluated forms as data
    Foreign(Arc<dyn Any + Send + Sync>),  // host-supplied opaque values
}

pub struct Closure {
    params: Vec<Arc<str>>,
    rest: Option<Arc<str>>,     // for (lambda (a b . rest) ...)
    body: Vec<Sexp>,
    captured_env: Env,
    source: SourceSpan,
}

pub struct NativeFn {
    name: Arc<str>,
    arity: Arity,
    callable: Arc<dyn Fn(&[Value], &mut HostCtx) -> Result<Value> + Send + Sync>,
}
```

`Foreign` is the primary mechanism for exposing host-owned Rust values (e.g.,
a `Job` handle, an `SnmpClient`) to Lisp code without marshalling through
primitive types. Native functions read `Foreign` values via downcast.

## Environment model

Eval-side `Env` mirrors tatara-lisp's `env::Env` shape but holds `Value`
instead of `Sexp`. Lexically scoped frames, captured by closures.

```rust
pub struct Env { /* frames: Vec<HashMap<Arc<str>, Value>>, parent: Option<...> */ }
```

Closures capture `Env` by `Arc`-clone at lambda-creation time. `set!` mutates
the captured frame, not the enclosing caller's frame — standard lexical
semantics.

## Host context

The evaluator is generic over a host context type `HostCtx` supplied by the
embedder:

```rust
pub struct Interpreter<H> {
    registry: FnRegistry<H>,
    globals: Env,
}

impl<H> Interpreter<H> {
    pub fn new() -> Self { /* ... */ }
    pub fn register_fn(&mut self, name: &str, arity: Arity, f: impl NativeCallable<H>) { /* ... */ }
    pub fn eval(&mut self, form: &Sexp, host: &mut H) -> Result<Value> { /* ... */ }
    pub fn repl_session(&mut self) -> ReplSession<H> { /* ... */ }
}
```

`hanshi` instantiates `Interpreter<HanshiCtx>` where `HanshiCtx` bundles the
SNMP client, job queue, event log, etc. Registered Rust functions have
`&mut HanshiCtx` access and can drive real IO.

## Special forms (in scope)

| Form | Semantics |
|------|-----------|
| `(quote x)` / `'x` | Literal; does not evaluate. Returns `Value::Sexp`. |
| `(if c t e)` / `(if c t)` | Two-arm conditional; e defaults to `Nil`. |
| `(cond (p1 e1) (p2 e2) ... (else en))` | First-match conditional. |
| `(when c body...)` / `(unless c body...)` | Sugar over `if`. |
| `(let ((x v) ...) body...)` | Parallel bindings. |
| `(let* ((x v) ...) body...)` | Sequential bindings. |
| `(letrec ((x v) ...) body...)` | Mutually-recursive bindings. |
| `(lambda (params) body...)` / `(lambda (a b . rest) body...)` | Closure literal. |
| `(define name val)` / `(define (name args) body...)` | Global or frame binding. |
| `(set! name val)` | Mutate existing binding in the nearest enclosing frame. |
| `(begin body...)` | Sequential eval, return last. |
| `(and e...)` / `(or e...)` | Short-circuiting. |
| `(not e)` | Logical negation. |

## Special forms (out of scope, v1)

- `define-syntax` / `syntax-rules` — macros stay in tatara-lisp's expander
- `call-with-current-continuation`
- `dynamic-wind`
- `delay` / `force`
- `do` loops (use recursion or native `for-each`)

## Primitive procedures (in scope)

Arithmetic: `+`, `-`, `*`, `/`, `modulo`, `quotient`, `remainder`, `abs`, `min`, `max`.
Comparison: `=`, `<`, `>`, `<=`, `>=`.
Predicates: `null?`, `pair?`, `list?`, `symbol?`, `string?`, `integer?`, `number?`, `boolean?`, `procedure?`, `foreign?`.
Lists: `car`, `cdr`, `cons`, `list`, `length`, `reverse`, `append`, `map`, `filter`, `fold`, `for-each`.
Equality: `eq?`, `eqv?`, `equal?`.
Strings: `string-length`, `string-append`, `substring`, `string->symbol`, `symbol->string`, `string->number`, `number->string`.
IO (sandboxable): `display`, `newline`, `print` — default to stdout; embedder can redirect.

Everything else (filesystem, network, time, process) is **not** a primitive;
embedders register it explicitly via FFI. This is the sandbox boundary.

## FFI surface

```rust
pub trait NativeCallable<H>: Send + Sync + 'static {
    fn call(&self, args: &[Value], host: &mut H) -> Result<Value>;
}

impl<H, F> NativeCallable<H> for F
where F: Fn(&[Value], &mut H) -> Result<Value> + Send + Sync + 'static { /* ... */ }

pub enum Arity {
    Exact(usize),
    AtLeast(usize),
    Range(usize, usize),
    Any,
}
```

Typed helpers for common cases:

```rust
// register a fn that takes (string, int) and returns Value:
interp.register_typed_fn("snmp-get", |host: &mut HanshiCtx, oid: &str, timeout_ms: i64| {
    host.snmp.get(oid, Duration::from_millis(timeout_ms as u64))
        .map(|resp| Value::from(resp))
});
```

Value ↔ Rust conversions via `From`/`TryFrom`:

```rust
impl From<i64> for Value { /* ... */ }
impl From<String> for Value { /* ... */ }
impl TryFrom<Value> for i64 { /* ... */ }
impl TryFrom<Value> for String { /* ... */ }
// etc.
```

For opaque host types:

```rust
interp.register_typed_fn("queue-enqueue", |host: &mut HanshiCtx, job: JobRef| {
    host.queue.enqueue(job.0)?;
    Ok(Value::Nil)
});

// JobRef wraps a Value::Foreign containing an Arc<Job>
```

## Error model

```rust
pub enum EvalError {
    UnboundSymbol { name: Arc<str>, at: SourceSpan },
    ArityMismatch { fn_name: Arc<str>, expected: Arity, got: usize, at: SourceSpan },
    TypeMismatch { expected: &'static str, got: &'static str, at: SourceSpan },
    DivisionByZero { at: SourceSpan },
    NotCallable { value_kind: &'static str, at: SourceSpan },
    BadSpecialForm { form: Arc<str>, reason: String, at: SourceSpan },
    NativeFn { name: Arc<str>, reason: String, at: SourceSpan },
    Halted,                     // host-initiated interrupt
}

pub struct SourceSpan { /* file: Option<Arc<str>>, start: Pos, end: Pos */ }
```

All errors carry a source span. The REPL catches `EvalError` and prints a
formatted trace; ordinary programs propagate via `Result<Value, EvalError>`.

**No panics in the evaluator.** A panic in a registered native fn is caught
at the FFI boundary and surfaces as `EvalError::NativeFn`.

## REPL surface

```rust
pub struct ReplSession<'i, H> {
    interp: &'i mut Interpreter<H>,
    globals: Env,              // persisted across forms
    source_name: Arc<str>,     // "repl" or a file for error messages
}

impl<'i, H> ReplSession<'i, H> {
    pub fn eval_str(&mut self, input: &str, host: &mut H) -> Result<Value, EvalError>;
    pub fn eval_stream<R: Read>(&mut self, r: R, host: &mut H) -> impl Iterator<Item = Result<Value, EvalError>>;
    pub fn is_complete(input: &str) -> bool;  // for multi-line UI — paren-balance check
}
```

`is_complete` enables remote REPL clients (e.g., hanshi's admin API) to know
when to submit a multi-line form.

## Consumer example — hanshi

```rust
// hanshi/crates/hanshi-core/src/lisp.rs
pub fn build_interpreter(ctx: &HanshiHost) -> Interpreter<HanshiHost> {
    let mut i = Interpreter::new();

    // expose the queue
    i.register_typed_fn("queue-peek", |h: &mut HanshiHost| {
        Ok(Value::List(Arc::new(h.queue.peek_all().into_iter().map(Value::from).collect())))
    });
    i.register_typed_fn("queue-enqueue", |h: &mut HanshiHost, job: JobRef| {
        h.queue.enqueue(job.0)?;
        Ok(Value::Nil)
    });

    // expose SNMP
    i.register_typed_fn("snmp-get", |h: &mut HanshiHost, oid: &str| {
        h.snmp.get(oid).map(Value::from)
    });

    // expose the job catalog (populated by tatara-lisp compile pass)
    i.register_typed_fn("job-by-name", |h: &mut HanshiHost, name: &str| {
        h.catalog.job_by_name(name).map(JobRef::from).map(Value::from)
            .ok_or_else(|| EvalError::native_fn("job-by-name", format!("no job: {name}")))
    });

    i
}

// REPL over Tailscale admin API
async fn repl_endpoint(State(ctx): State<HanshiHost>, body: String) -> Json<ReplResponse> {
    let mut session = ctx.interp.repl_session();
    match session.eval_str(&body, &mut ctx.clone()) {
        Ok(v) => Json(ReplResponse::Ok(v.to_string())),
        Err(e) => Json(ReplResponse::Err(e.to_string())),
    }
}
```

## Test strategy

- **Unit tests per module**: reader-output → evaluator for each special form;
  FFI round-trip (register Rust fn, call from Lisp, check return).
- **Golden tests** under `tests/golden/`: source `.tl` file + expected output.
  Catches semantic regressions.
- **Scheme-standard test cases**: subset of the R5RS test suite for the
  primitives we claim to support. Lifted from Chez/Racket golden expectations.
- **Property tests** (proptest): arithmetic commutativity, list reverse-inverse,
  env shadowing invariants.
- **REPL session tests**: feed stream of forms, assert persistent state
  across forms.

## Open questions

1. **Tail-call optimization**: not in v1 (simpler evaluator). Orchestration
   code rarely deep-recurses. Revisit if any rule accumulates stack.
2. **Concurrency**: evaluator is single-threaded. Async-native would complicate
   significantly. If a registered fn is async, the FFI layer can block-on or
   the evaluator can become `async` — defer until hanshi forces the issue.
3. **Memory**: `Value` uses `Arc` throughout for cheap clone. No GC needed;
   `Arc` cycles are avoided because closures only capture lexical envs which
   do not back-reference themselves. Watch for mutual recursion via `letrec`
   — implemented with `Arc<RefCell<Option<Value>>>` slot-and-patch.
4. **Sandboxing vs FFI trust**: v1 trusts the embedder. Native fns registered
   by the embedder are considered privileged. If hanshi wants to accept
   user-supplied Lisp via public API, it must curate the function set exposed
   to that interpreter — easy via a second `Interpreter` with a reduced
   registry.
5. **Source spans through macro expansion**: spans from `reader::read` need
   to survive `macro_expand`. Requires a small upstream change to tatara-lisp
   to thread spans — tracked separately.

## Scope discipline

Everything above is the v1 surface. Additions require a follow-up design doc.
This keeps the evaluator small enough that it can be audited in one sitting —
a prerequisite for trusting it to drive a $30k machine.
