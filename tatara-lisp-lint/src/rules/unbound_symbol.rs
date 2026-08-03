//! `unbound-symbol` — flag references to names nothing binds, at LINT time.
//!
//! WHY THIS RULE EXISTS (measured 2026-08-02): `tatara-script lint` reported
//! `1 file(s) scanned, 0 violation(s), 0 parse error(s)` on a script whose
//! second line called `string-downcase` — a primitive that does not exist. The
//! file parsed, so every existing rule was happy; the failure surfaced only when
//! the interpreter reached the symbol at RUNTIME.
//!
//! That gap is load-bearing rather than cosmetic, because tlisp's most important
//! deployment shape is a hook: `blackmatter.components.gitconfig` installs a
//! tatara-script `commit-msg` hook via `core.hooksPath`, so ONE typo in it
//! blocks every commit in every repo on the machine — including the commit that
//! would fix it. "Lints clean, dies at runtime" is exactly the wrong failure
//! mode for that, and a guard nobody can trust to be pre-flighted is a guard
//! that stays un-deployed.
//!
//! THE SOURCE OF TRUTH IS THE INTERPRETER, NEVER A LIST IN THIS CRATE.
//! `known` is injected by the caller, which builds a real `Interpreter`, runs
//! the same `install_stdlib` the runtime uses, and hands over
//! `reserved_head_names()` + its globals. A hardcoded table here would drift
//! from the interpreter the first time a primitive landed — reproducing the
//! `Co-Authored-By` / `Claude-Session` split that this repo already paid for
//! once (a guard catching one member of a pair reads exactly like a guard
//! catching the pair). `tatara-lisp-lint` depends only on `tatara-lisp`, so the
//! dependency is inverted deliberately: the rule cannot reach the interpreter,
//! and therefore cannot keep a stale copy of it.
//!
//! FALSE POSITIVES ARE THE WHOLE DESIGN RISK. A lint that cries wolf gets
//! ignored, and an ignored lint is worse than no lint because it still costs a
//! run. So the rule is deliberately conservative:
//!   * quoted data (`'(a b c)`) is skipped entirely — those symbols are data,
//!     not references. Inside a quasiquote only `unquote` / `unquote-splicing`
//!     subtrees are evaluated, so only those are walked.
//!   * every binding form's names are honoured: `define` in both shapes,
//!     `lambda` params (including a rest symbol), and `let` / `let*` / `letrec`.
//!   * ALL top-level definitions are collected in a first pass, so mutual
//!     recursion and use-before-define never report.
//!   * a program containing `(require …)` is skipped WHOLESALE — imported
//!     bindings are invisible from here, so reporting would be noise. This is a
//!     real hole, stated rather than hidden: the rule protects standalone
//!     scripts (the hook shape) and abstains where it cannot see.
//!   * keywords (`:foo`) and literals are never references.
//! Suppress a deliberate case with `;; lint:allow unbound-symbol <reason>`.
//!
//! ★ THE REMAINING FALSE-POSITIVE FLOOR IS STRUCTURAL, NOT A MISSING CASE LIST.
//! Fleet sweep over 27 `.tlisp` files: 422 → 87 after handling the `defmacro`
//! three-part shape, lambda-list keywords, generic `def…` heads, the `fn` / `λ`
//! aliases and `catch`. The residual 87 are all ONE class, and no amount of
//! syntactic special-casing closes it:
//!
//!   * BINDING FORMS THAT ARE THEMSELVES USER MACROS — `(dolist (entry xs) …)`,
//!     `(when-let (f expr) …)`, `(if-let …)`, `(with-gensyms (loop) …)`. Each
//!     binds a name, and which forms bind is decided by a `defmacro` that may
//!     live in another file. A purely syntactic pass cannot know.
//!   * MACRO-DSL DATA — `(defreversal decouple :losses nothing)`. `nothing` is a
//!     symbolic constant consumed by the macro, never evaluated. Indistinguishable
//!     from a reference without knowing the macro.
//!
//! Both dissolve at the same place: run this rule over MACRO-EXPANDED forms
//! rather than raw source. `Interpreter::fully_expand` already exists and is
//! what the runtime itself uses, so the expansion is authoritative rather than a
//! second guess — the same "ask the interpreter, don't reimplement it" move that
//! `known` already makes for primitives. That is the fix that would let this rule
//! go default-on; enumerating more binding heads here would not, because the set
//! is open by construction. Until then the rule stays opt-in (`--unbound`) and is
//! FP-clean on standalone, macro-light scripts — the hook shape it was built for.
//!
//! One further honest limit, unrelated to macros: a file meant to be CONCATENATED
//! before running (substrate's `aferir.test.tlisp` documents
//! `cat aferir.tlisp aferir.test.tlisp > /tmp/t.tlisp`) genuinely does not bind
//! its own helpers. Those reports are CORRECT about the file and useless about
//! the program. There is no file-wide suppression today.

use std::collections::BTreeSet;

use tatara_lisp::{Atom, Spanned, SpannedForm};

use crate::{head_symbol, line_col, suppressed, Rule, Violation};

/// Syntactic heads that are never *value* references, independent of whatever
/// the interpreter registers. Kept tiny and local: these are reader/special-form
/// keywords (`else` is `cond`'s, not a binding), so a rule run stays sane even
/// if a caller passes an empty `known` set.
const SYNTAX: &[&str] = &[
    "define",
    "defmacro",
    // Lambda-list keywords. `&rest` is a marker in a parameter list, never a
    // value reference — it accounted for 20 of the 422 false positives in the
    // first fleet sweep.
    "&rest",
    "&optional",
    "&body",
    "&key",
    "&aux",
    "&whole",
    "lambda",
    "let",
    "let*",
    "letrec",
    "quote",
    "quasiquote",
    "unquote",
    "unquote-splicing",
    "if",
    "when",
    "unless",
    "cond",
    "case",
    "else",
    "begin",
    "and",
    "or",
    "set!",
    "require",
];

/// Flags evaluated references to names that neither the injected environment nor
/// the program itself binds.
pub struct UnboundSymbol {
    known: BTreeSet<String>,
}

/// Build the rule over the caller's environment. Pass the interpreter's own
/// `reserved_head_names()` plus its global names — see the module docs for why
/// this is a parameter and not a constant.
#[must_use]
pub fn unbound_symbol<I: IntoIterator<Item = String>>(known: I) -> UnboundSymbol {
    UnboundSymbol {
        known: known.into_iter().collect(),
    }
}

/// Names a binding form introduces, given the form's items.
fn binder_names(items: &[Spanned]) -> Vec<String> {
    let mut out = Vec::new();
    // Any `def…` head: `define`'s two shapes, plus `defmacro` / `deftest` /
    // user-macro forms. Pass 1 uses only the FIRST name returned here, so
    // covering every definition head is what stops a top-level macro from
    // reporting at its own call sites.
    if head_symbol(items).is_some_and(|h| h.starts_with("def")) {
        if let Some(target) = items.get(1) {
            match &target.form {
                SpannedForm::Atom(Atom::Symbol(s)) => out.push(s.clone()),
                SpannedForm::List(sig) => out.extend(symbol_names(sig)),
                _ => {}
            }
        }
    }
    out
}

/// Every plain symbol in a flat list — used for lambda/define parameter lists.
fn symbol_names(items: &[Spanned]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match &i.form {
            SpannedForm::Atom(Atom::Symbol(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// `let`-family bindings: `((name value) ...)`, tolerating `(name)`.
fn let_binding_pairs(node: &Spanned) -> Vec<(String, Option<&Spanned>)> {
    let SpannedForm::List(bindings) = &node.form else {
        return Vec::new();
    };
    bindings
        .iter()
        .filter_map(|b| match &b.form {
            SpannedForm::List(pair) => match pair.first().map(|p| &p.form) {
                Some(SpannedForm::Atom(Atom::Symbol(s))) => Some((s.clone(), pair.get(1))),
                _ => None,
            },
            // `(let (x) ...)` — a bare name with no initialiser.
            SpannedForm::Atom(Atom::Symbol(s)) => Some((s.clone(), None)),
            _ => None,
        })
        .collect()
}

/// True when any form anywhere in the program is a `(require …)`.
fn uses_require(node: &Spanned) -> bool {
    match &node.form {
        SpannedForm::List(items) => {
            if head_symbol(items) == Some("require") {
                return true;
            }
            items.iter().any(uses_require)
        }
        SpannedForm::Quote(inner)
        | SpannedForm::Quasiquote(inner)
        | SpannedForm::Unquote(inner)
        | SpannedForm::UnquoteSplice(inner) => uses_require(inner),
        _ => false,
    }
}

impl UnboundSymbol {
    fn resolves(&self, name: &str, scopes: &[BTreeSet<String>]) -> bool {
        self.known.contains(name)
            || SYNTAX.contains(&name)
            || scopes.iter().any(|s| s.contains(name))
    }

    /// Walk only the evaluated parts of a quasiquote — the unquoted holes.
    fn walk_quasi(
        &self,
        node: &Spanned,
        scopes: &mut Vec<BTreeSet<String>>,
        out: &mut Vec<(usize, String)>,
    ) {
        match &node.form {
            SpannedForm::Unquote(inner) | SpannedForm::UnquoteSplice(inner) => {
                self.walk(inner, scopes, out);
            }
            SpannedForm::List(items) => {
                for i in items {
                    self.walk_quasi(i, scopes, out);
                }
            }
            SpannedForm::Quasiquote(inner) => self.walk_quasi(inner, scopes, out),
            _ => {}
        }
    }

    fn walk(
        &self,
        node: &Spanned,
        scopes: &mut Vec<BTreeSet<String>>,
        out: &mut Vec<(usize, String)>,
    ) {
        match &node.form {
            SpannedForm::Atom(Atom::Symbol(name)) => {
                if !self.resolves(name, scopes) {
                    out.push((node.span.start, name.clone()));
                }
            }
            // Quoted data is data. Never a reference.
            SpannedForm::Quote(_) => {}
            SpannedForm::Quasiquote(inner) => self.walk_quasi(inner, scopes, out),
            SpannedForm::Unquote(inner) | SpannedForm::UnquoteSplice(inner) => {
                self.walk(inner, scopes, out);
            }
            SpannedForm::List(items) => self.walk_list(items, scopes, out),
            SpannedForm::Atom(_) | SpannedForm::Nil => {}
        }
    }

    fn walk_list(
        &self,
        items: &[Spanned],
        scopes: &mut Vec<BTreeSet<String>>,
        out: &mut Vec<(usize, String)>,
    ) {
        match head_symbol(items) {
            Some("quote") => {}

            Some("define") => {
                // The bound name belongs to the ENCLOSING scope; parameters to a
                // fresh inner one. `define`'s third item is a VALUE, never a
                // parameter list — which is exactly why it cannot share the
                // generic `def*` arm below.
                let mut names = binder_names(items);
                let params: Vec<String> = if names.len() > 1 { names.split_off(1) } else { vec![] };
                if let Some(scope) = scopes.last_mut() {
                    scope.extend(names);
                }
                scopes.push(params.into_iter().collect());
                for item in items.iter().skip(2) {
                    self.walk(item, scopes, out);
                }
                scopes.pop();
            }

            // Every OTHER `def…` head: `defmacro`, plus driver and user-macro
            // definition forms (`deftest`, `defcheck`, `defphase`,
            // `defreversal`, `defsuite`, …).
            //
            // This arm exists because the first fleet sweep's false positives
            // were overwhelmingly ONE shape: `(defmacro -> (x &rest steps) …)`,
            // i.e. NAME and PARAMS as separate items. Treating that like
            // `define` left every macro parameter unbound — `name`, `body`,
            // `binding`, `x`, `step`, `steps`, `params` were 100+ reports on
            // their own.
            //
            // Handled generically rather than by listing known heads, because
            // the set is open: `deftest` is a driver form the interpreter never
            // registers, and the others are user macros that may be defined in a
            // file we are not looking at. A `def…` head is therefore never
            // itself reported. The cost is a typo'd `def…` head going unnoticed;
            // that is the right trade, since lint cannot verify those heads
            // exist anyway.
            Some(head) if head.starts_with("def") => {
                let mut inner: BTreeSet<String> = BTreeSet::new();
                match items.get(1).map(|t| &t.form) {
                    // (defmacro NAME (params…) body…)
                    Some(SpannedForm::Atom(Atom::Symbol(s))) => {
                        if let Some(scope) = scopes.last_mut() {
                            scope.insert(s.clone());
                        }
                    }
                    // (defthing (NAME params…) body…)
                    Some(SpannedForm::List(sig)) => {
                        let names = symbol_names(sig);
                        if let Some((first, rest)) = names.split_first() {
                            if let Some(scope) = scopes.last_mut() {
                                scope.insert(first.clone());
                            }
                            inner.extend(rest.iter().cloned());
                        }
                    }
                    // A string-named form, e.g. (deftest "does a thing" body…).
                    _ => {}
                }

                // A list of ONLY symbols directly after the name is a parameter
                // list; anything else is body. This is what separates
                // `(defmacro -> (x &rest steps) …)` from a form whose third item
                // is data. When in doubt it treats the list as params, which can
                // only ever SUPPRESS a report, never invent one.
                let mut body_start = 2;
                if let Some(SpannedForm::List(sig)) = items.get(2).map(|p| &p.form) {
                    let names = symbol_names(sig);
                    if !sig.is_empty() && names.len() == sig.len() {
                        inner.extend(names);
                        body_start = 3;
                    }
                }

                scopes.push(inner);
                for item in items.iter().skip(body_start) {
                    self.walk(item, scopes, out);
                }
                scopes.pop();
            }

            // `fn` and `λ` are lambda aliases and are used more than `lambda`
            // itself in places — `(fn (acc t) …)` in a `reduce` accounted for a
            // large share of the residual false positives. `catch` is the same
            // SHAPE: `(catch (e) handler…)` binds its parameter list exactly like
            // a lambda does.
            Some("lambda" | "fn" | "λ" | "catch") => {
                let params = match items.get(1).map(|p| &p.form) {
                    Some(SpannedForm::List(sig)) => symbol_names(sig),
                    // `(lambda rest body)` — variadic with a single rest name.
                    Some(SpannedForm::Atom(Atom::Symbol(s))) => vec![s.clone()],
                    _ => vec![],
                };
                scopes.push(params.into_iter().collect());
                for item in items.iter().skip(2) {
                    self.walk(item, scopes, out);
                }
                scopes.pop();
            }

            Some(kind @ ("let" | "let*" | "letrec")) => {
                let pairs = items.get(1).map(let_binding_pairs).unwrap_or_default();
                // `let` evaluates every initialiser in the OUTER scope; `let*`
                // and `letrec` see the names bound so far (letrec sees all of
                // them, which the incremental walk approximates safely — it can
                // only ever report fewer, never more).
                if kind == "let" {
                    for (_, init) in &pairs {
                        if let Some(v) = init {
                            self.walk(v, scopes, out);
                        }
                    }
                    scopes.push(pairs.into_iter().map(|(n, _)| n).collect());
                } else {
                    scopes.push(BTreeSet::new());
                    for (name, init) in pairs {
                        if let Some(v) = init {
                            self.walk(v, scopes, out);
                        }
                        if let Some(scope) = scopes.last_mut() {
                            scope.insert(name);
                        }
                    }
                }
                for item in items.iter().skip(2) {
                    self.walk(item, scopes, out);
                }
                scopes.pop();
            }

            // Ordinary application (or a form this rule has no special
            // knowledge of): head and every argument are references.
            _ => {
                for item in items {
                    self.walk(item, scopes, out);
                }
            }
        }
    }
}

impl Rule for UnboundSymbol {
    fn name(&self) -> &'static str {
        "unbound-symbol"
    }

    fn description(&self) -> &'static str {
        "a symbol is referenced that neither the interpreter nor the program binds — it would fail at runtime, not at parse time"
    }

    fn check(&self, forms: &[Spanned], src: &str) -> Vec<Violation> {
        // Abstain rather than guess when bindings can arrive from a module.
        if forms.iter().any(uses_require) {
            return Vec::new();
        }

        // Pass 1: every top-level definition, so order and mutual recursion
        // never matter.
        let mut top: BTreeSet<String> = BTreeSet::new();
        for form in forms {
            if let SpannedForm::List(items) = &form.form {
                top.extend(binder_names(items).into_iter().take(1));
            }
        }

        // Pass 2: report unresolved references.
        let mut found = Vec::new();
        let mut scopes = vec![top];
        for form in forms {
            self.walk(form, &mut scopes, &mut found);
        }

        found
            .into_iter()
            .filter(|(byte, _)| !suppressed(src, *byte, self.name()))
            .map(|(byte, name)| {
                let (line, col) = line_col(src, byte);
                Violation {
                    rule: self.name(),
                    line,
                    col,
                    message: format!(
                        "`{name}` is not bound by the interpreter or this program. It parses fine \
                         and fails only when evaluation reaches it — check the spelling against the \
                         installed primitives (e.g. `string-lowercase`, not `string-downcase`), or \
                         define it. Suppress with `;; lint:allow unbound-symbol <reason>`."
                    ),
                }
            })
            .collect()
    }
}
