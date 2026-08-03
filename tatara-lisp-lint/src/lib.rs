//! `tatara-lisp-lint` — a typed, pluggable rule engine that lints `.tlisp`
//! source against semantic [`Rule`]s evaluated over the real `tatara-lisp`
//! AST (via [`tatara_lisp::read_spanned`]).
//!
//! Adding a fleet-wide check is one constructor + one line in [`default_rules`].
//! The seed rules are instances of [`rules::CommandMutationDiscarded`]
//! ([`rules::git_mutation_discarded`], [`rules::gh_mutation_discarded`]): they
//! make the 2026-06-03 auto-release silent-failure class mechanically
//! un-reintroducible — a *discarded* `(exec-check …)` / `(exec-capture …)` of a
//! state-changing `git` / `gh` command is a violation, because `exec-check`
//! returns the exit code rather than raising, so discarding it silently ignores
//! failure.
//!
//! The load-bearing reusable primitive is [`walk_consumption`]: a
//! consumption-aware AST walk that tells a rule, for every node, whether that
//! node's *value* is used by its parent or evaluated only for effect and
//! discarded. Future "result must be used" rules reuse it verbatim.
//!
//! Suppress an intentional discard with an inline comment on the same or the
//! preceding line: `;; lint:allow git-mutation-discard <reason>`.

use tatara_lisp::{read_spanned, Atom, Spanned, SpannedForm};

pub mod rules;

/// One rule violation, located by 1-based line/column into the linted source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The [`Rule::name`] that produced this violation.
    pub rule: &'static str,
    /// 1-based line of the offending form.
    pub line: usize,
    /// 1-based column of the offending form.
    pub col: usize,
    /// Human-facing explanation + remedy.
    pub message: String,
}

/// A semantic lint rule over a parsed tlisp program. Stateless by construction:
/// implementors inspect the spanned form tree (plus the raw `src`, for line
/// mapping + suppression) and return violations.
pub trait Rule: Send + Sync {
    /// Stable kebab-case identifier, e.g. `"git-mutation-discard"`. Used in
    /// output and in `lint:allow <name>` suppression comments.
    fn name(&self) -> &'static str;
    /// One-line description of what the rule enforces.
    fn description(&self) -> &'static str;
    /// Inspect the program and collect violations.
    fn check(&self, forms: &[Spanned], src: &str) -> Vec<Violation>;
}

/// The default rule set run by `tatara-script lint`. Append a `Box::new(...)`
/// here to ship a new check fleet-wide — that one line is the whole adoption
/// surface for a new rule.
#[must_use]
pub fn default_rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(rules::git_mutation_discarded()),
        Box::new(rules::gh_mutation_discarded()),
    ]
}

/// Parse `src` and run every rule, returning violations sorted by position.
///
/// # Errors
/// Returns the parser error string when `src` does not parse — surfaced so a
/// rule run never silently no-ops on unparseable input (the balance/parse
/// layer of `tlisp-lint` already reports that class to the operator too).
pub fn lint_source(src: &str, rules: &[Box<dyn Rule>]) -> Result<Vec<Violation>, String> {
    let forms = read_spanned(src).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for rule in rules {
        out.extend(rule.check(&forms, src));
    }
    out.sort_by(|a, b| (a.line, a.col).cmp(&(b.line, b.col)));
    Ok(out)
}

/// Run every rule over forms the CALLER has already parsed (and possibly
/// transformed), rather than re-parsing `src`.
///
/// Exists so the binary can macro-EXPAND before linting. Several shapes are
/// undecidable on raw source — `(dolist (entry xs) …)` binds `entry`, but which
/// forms bind is decided by a `defmacro` that may live anywhere — and they
/// simply disappear once the macro is expanded. `src` is still required: rules
/// map spans back to it for line/column and read `lint:allow` suppressions, so
/// it must be the ORIGINAL text even when `forms` are expanded.
///
/// # Errors
/// Never returns `Err` today; the signature matches [`lint_source`] so callers
/// can swap one for the other.
pub fn lint_forms(
    forms: &[Spanned],
    src: &str,
    rules: &[Box<dyn Rule>],
) -> Result<Vec<Violation>, String> {
    let mut out = Vec::new();
    for rule in rules {
        out.extend(rule.check(forms, src));
    }
    out.sort_by(|a, b| (a.line, a.col).cmp(&(b.line, b.col)));
    Ok(out)
}

/// Map a byte offset into `src` to a 1-based `(line, col)`.
#[must_use]
pub fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(src.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in src.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// True when the line containing `byte` (or the immediately preceding line)
/// carries an inline `lint:allow <rule>` suppression for `rule`.
#[must_use]
pub fn suppressed(src: &str, byte: usize, rule: &str) -> bool {
    if suppressed_file_wide(src, rule) {
        return true;
    }
    let (line, _) = line_col(src, byte);
    let mut needle = String::from("lint:allow ");
    needle.push_str(rule);
    src.lines().enumerate().any(|(idx, text)| {
        let n = idx + 1;
        (n == line || n + 1 == line) && text.contains(&needle)
    })
}

/// Whole-file suppression: `;; lint:allow-file <rule> <reason>` in the header.
///
/// Per-line suppression cannot express "this FILE is not self-contained", which
/// is the one remaining class of `unbound-symbol` false positives after macro
/// expansion. Two real shapes, both measured 2026-08-02:
///   * a fragment meant to be CONCATENATED before running (substrate's
///     `aferir.test.tlisp`: `cat aferir.tlisp aferir.test.tlisp > t.tlisp`);
///   * a DSL whose macros are supplied by the DRIVER, not the file (nix's
///     `pkgs/tebiki/runbooks/*.tlisp` — `defreversal` and friends, zero
///     `defmacro` in-file, so nothing can register them).
/// Both are CORRECT about the file and useless about the program. Annotating
/// them per-line would mean dozens of comments that rot; one header line says
/// the true thing once.
///
/// Deliberately confined to the HEADER — the first 20 lines, before any code —
/// so it reads as a property of the file rather than something buried mid-way
/// that silently disables a rule for everything below it.
#[must_use]
pub fn suppressed_file_wide(src: &str, rule: &str) -> bool {
    let mut needle = String::from("lint:allow-file ");
    needle.push_str(rule);
    src.lines().take(20).any(|text| text.contains(&needle))
}

/// Head symbol of a list form, if its first element is a plain symbol.
#[must_use]
pub fn head_symbol(items: &[Spanned]) -> Option<&str> {
    match items.first().map(|s| &s.form) {
        Some(SpannedForm::Atom(Atom::Symbol(s))) => Some(s.as_str()),
        _ => None,
    }
}

/// The string-literal value of a node, if it is a string atom.
#[must_use]
pub fn string_lit(node: &Spanned) -> Option<&str> {
    match &node.form {
        SpannedForm::Atom(Atom::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Visit every form in the tree, invoking `f(node, consumed)` where `consumed`
/// is true iff the node's VALUE is used by its parent (vs evaluated only for
/// effect and discarded).
///
/// Models the sequencing semantics of `define` / `let` / `let*` / `begin` /
/// `when` / `unless` / `cond` / `if` / `lambda`; every other head treats its
/// arguments as consumed. A function/lambda body's TAIL is `consumed = true`
/// (the return value escapes to callers, which may check it). Conservative by
/// design: it never reports a consumed node as discarded, so rules built on it
/// do not false-positive — they may only under-report exotic shapes.
pub fn walk_consumption(node: &Spanned, consumed: bool, f: &mut dyn FnMut(&Spanned, bool)) {
    f(node, consumed);
    match &node.form {
        SpannedForm::List(items) if !items.is_empty() => {
            walk_list(items, consumed, f);
        }
        SpannedForm::Quote(inner)
        | SpannedForm::Quasiquote(inner)
        | SpannedForm::Unquote(inner)
        | SpannedForm::UnquoteSplice(inner) => {
            // Quoted data is not evaluated — descend as consumed so nothing
            // inside a quote is ever reported as a discarded call.
            walk_consumption(inner, true, f);
        }
        _ => {}
    }
}

/// Walk the children of a non-empty list, dispatching on its head form.
fn walk_list(items: &[Spanned], consumed: bool, f: &mut dyn FnMut(&Spanned, bool)) {
    match head_symbol(items) {
        Some("cond") => {
            // (cond (test body...) ...) — each clause is a (test body...) list
            // that is NOT itself an evaluated call.
            for clause in &items[1..] {
                match &clause.form {
                    SpannedForm::List(c) if !c.is_empty() => {
                        walk_consumption(&c[0], true, f); // test value used
                        walk_seq(&c[1..], consumed, f); // body: tail = cond's value
                    }
                    _ => walk_consumption(clause, consumed, f),
                }
            }
        }
        Some("if") => {
            if let Some(test) = items.get(1) {
                walk_consumption(test, true, f);
            }
            for branch in items.iter().skip(2) {
                walk_consumption(branch, consumed, f);
            }
        }
        Some("when" | "unless") => {
            if let Some(test) = items.get(1) {
                walk_consumption(test, true, f);
            }
            walk_seq(items.get(2..).unwrap_or(&[]), consumed, f);
        }
        Some("begin") => walk_seq(items.get(1..).unwrap_or(&[]), consumed, f),
        Some("let" | "let*") => {
            // (let (bindings) body...) or named (let name (bindings) body...)
            let named = matches!(
                items.get(1).map(|s| &s.form),
                Some(SpannedForm::Atom(Atom::Symbol(_)))
            );
            let bind_idx = if named { 2 } else { 1 };
            if let Some(SpannedForm::List(binds)) = items.get(bind_idx).map(|s| &s.form) {
                for b in binds {
                    if let SpannedForm::List(pair) = &b.form {
                        for expr in pair.iter().skip(1) {
                            walk_consumption(expr, true, f); // bound value used
                        }
                    }
                }
            }
            walk_seq(items.get(bind_idx + 1..).unwrap_or(&[]), consumed, f);
        }
        Some("lambda") => {
            // (lambda (params) body...) — body tail escapes as the return value.
            walk_seq(items.get(2..).unwrap_or(&[]), true, f);
        }
        Some("define") => {
            if matches!(items.get(1).map(|s| &s.form), Some(SpannedForm::List(_))) {
                // (define (sig) body...) — body tail is the fn's return value.
                walk_seq(items.get(2..).unwrap_or(&[]), true, f);
            } else {
                for value in items.iter().skip(2) {
                    walk_consumption(value, true, f); // (define name value)
                }
            }
        }
        // Function/macro call (incl. exec-check / exec-capture / exec-or-die /
        // list / = / and / or / not / status-of / …) and computed-operator
        // calls: every argument's value is used.
        _ => {
            for child in items.iter().skip(1) {
                walk_consumption(child, true, f);
            }
            // A computed operator (head is itself a list/lambda) must be walked.
            if head_symbol(items).is_none() {
                if let Some(op) = items.first() {
                    walk_consumption(op, true, f);
                }
            }
        }
    }
}

/// Walk a sequence body: every element but the last is evaluated for effect
/// (discarded); the last inherits `tail_consumed`.
fn walk_seq(body: &[Spanned], tail_consumed: bool, f: &mut dyn FnMut(&Spanned, bool)) {
    let n = body.len();
    for (i, form) in body.iter().enumerate() {
        walk_consumption(form, i + 1 == n && tail_consumed, f);
    }
}
