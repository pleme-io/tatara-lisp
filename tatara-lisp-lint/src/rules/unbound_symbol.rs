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
    match head_symbol(items) {
        // (define NAME v) | (define (NAME . params) body...)
        Some("define" | "defmacro") => {
            if let Some(target) = items.get(1) {
                match &target.form {
                    SpannedForm::Atom(Atom::Symbol(s)) => out.push(s.clone()),
                    SpannedForm::List(sig) => out.extend(symbol_names(sig)),
                    _ => {}
                }
            }
        }
        _ => {}
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

            Some("define" | "defmacro") => {
                // The bound name belongs to the ENCLOSING scope; parameters to a
                // fresh inner one.
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

            Some("lambda") => {
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
