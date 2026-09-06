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
use std::fmt;

use tatara_lisp::{Atom, Spanned, SpannedForm};

use crate::{head_symbol, line_col, suppressed, Rule, Violation};

// ── THE SHAPE CATALOG ────────────────────────────────────────────────────────
//
// Every form shape this rule can meet, each with a PRESCRIBED handling — including
// the shapes we cannot resolve, whose prescription is an explicit abstention
// rather than silence. Enumerated rather than implicit so that "which shapes are
// handled" is answerable by reading one table instead of the walker, and so a new
// shape cannot land without a row (see `tests/unbound_symbol.rs`:
// `catalog_covers_every_special_cased_head` and `every_shape_has_a_matrix_case`).
//
// The head lists below are the SINGLE SOURCE for both the walker's match arms and
// this catalog, so an arm cannot drift from its row — the same discipline `known`
// applies to primitives, one level up.

// The head lists themselves now live in `tatara_lisp::binding_shapes`, the
// crate BOTH syntactic walkers in this workspace already depend on — this rule
// and `tatara_lisp_eval::build_check`'s arity checker, which needs the same
// answer to "does this head shadow a name?" and would otherwise carry a second
// copy free to disagree. Re-exported here (rather than referenced inline at the
// match arms) so the catalog rows below and the walker keep reading ONE
// identifier apiece, exactly as before.

/// `(define NAME v)` / `(define (NAME params…) body…)`. Third item is a VALUE,
/// which is why `define` cannot share the generic `def…` arm.
const DEFINE_HEADS: &[&str] = tatara_lisp::binding_shapes::DEFINE_HEADS;

/// Lambda-shaped: a parameter list (or a bare rest symbol) followed by a body.
/// `fn` / `λ` are aliases; `catch` binds `(e)` exactly the same way.
const LAMBDA_HEADS: &[&str] = tatara_lisp::binding_shapes::LAMBDA_HEADS;

/// `((name init) …)` bindings then a body. `let` evaluates initialisers in the
/// OUTER scope; `let*` / `letrec` see the names bound so far.
const LET_HEADS: &[&str] = tatara_lisp::binding_shapes::LET_HEADS;

/// Contents are data, never references.
const QUOTE_HEADS: &[&str] = tatara_lisp::binding_shapes::QUOTE_HEADS;

/// Prefix for every other definition form — `defmacro`, plus driver and
/// user-macro heads (`deftest`, `defphase`, `defreversal`, …). A prefix rather
/// than a list because the set is OPEN: those heads may be defined in a file we
/// are not looking at, or by the test driver rather than the interpreter.
const DEF_PREFIX: &str = tatara_lisp::binding_shapes::DEF_PREFIX;

/// What the rule does with a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prescription {
    /// Head and every item are ordinary value references.
    Application,
    /// Introduces names; the walker tracks them so they never report.
    Binds,
    /// Contents are data — not walked at all.
    Data,
    /// Data except for evaluated holes, which are walked.
    DataWithHoles,
    /// Cannot be resolved syntactically. The rule deliberately reports NOTHING
    /// for the whole program rather than guessing.
    Abstain,
    /// The RULE ALONE reports here; the CALLER dissolves it by macro-expanding
    /// before linting (`tatara-script lint` does). Split from `Binds` because the
    /// distinction is real and load-bearing: an embedder calling `lint_source` on
    /// raw text still sees these, so the honest answer depends on who is asking.
    ResolvedByCallerExpansion,
    /// Known to produce FALSE POSITIVES today. Recorded so the limit is a row in
    /// a table rather than folklore, and pinned by a characterization test so a
    /// future fix visibly flips it.
    UnresolvedFalsePositive,
}

/// One catalog row.
pub struct Shape {
    /// Stable identifier, also the key a matrix case cites.
    pub name: &'static str,
    /// Heads this row governs; empty when the row is structural (application,
    /// quasiquote, whole-program abstention) or prefix-matched.
    pub heads: &'static [&'static str],
    pub prescription: Prescription,
    pub example: &'static str,
    /// Why this prescription — and for the unresolved rows, what would fix it.
    pub note: &'static str,
    /// Name of the matrix case in `tests/unbound_symbol.rs` that proves this row.
    /// Test-enforced to exist, so a shape cannot be catalogued without evidence.
    pub covered_by: &'static str,
}

/// The catalog. Adding a shape to the walker means adding a row here and a matrix
/// case naming it; both are test-enforced.
pub const SHAPES: &[Shape] = &[
    Shape {
        name: "application",
        heads: &[],
        prescription: Prescription::Application,
        example: "(string-lowercase x)",
        note: "The default. Head and arguments are all references; this is what makes a typo detectable at all.",
        covered_by: "the measured 2026-08-02 miss: a primitive that does not exist",
    },
    Shape {
        name: "define",
        heads: DEFINE_HEADS,
        prescription: Prescription::Binds,
        example: "(define (f x) x)",
        note: "Name into the enclosing scope, parameters into a fresh inner one. Kept separate from def* because item 3 is a value, not a parameter list.",
        covered_by: "define function shorthand binds name AND params",
    },
    Shape {
        name: "def-family",
        heads: &[],
        prescription: Prescription::Binds,
        example: "(defmacro -> (x &rest steps) ...)",
        note: "Prefix-matched on `def`. Covers defmacro's three-part shape plus driver/user-macro heads. The head is never itself reported, because lint cannot verify heads it did not see defined.",
        covered_by: "defmacro three-part shape binds NAME and PARAMS separately",
    },
    Shape {
        name: "lambda-shaped",
        heads: LAMBDA_HEADS,
        prescription: Prescription::Binds,
        example: "(fn (acc t) (list acc t))",
        note: "Parameter list or bare rest symbol, then a body. `fn`/`λ` are aliases; `catch` is the same shape.",
        covered_by: "`fn` is a lambda alias",
    },
    Shape {
        name: "let-family",
        heads: LET_HEADS,
        prescription: Prescription::Binds,
        example: "(let* ((a 1) (b a)) b)",
        note: "`let` evaluates initialisers in the outer scope — so `(let ((a a)) …)` correctly reports. `let*`/`letrec` bind incrementally.",
        covered_by: "let* sees earlier bindings",
    },
    Shape {
        name: "quote",
        heads: QUOTE_HEADS,
        prescription: Prescription::Data,
        example: "'(alpha beta)",
        note: "Symbols inside are data. Walking them would report every quoted list in the fleet.",
        covered_by: "quoted list is data",
    },
    Shape {
        name: "quasiquote",
        heads: &[],
        prescription: Prescription::DataWithHoles,
        example: "`(a ,x)",
        note: "Only unquote / unquote-splicing subtrees are evaluated, so only those are walked. This is what lets macro templates be checked without false-reporting their literal structure.",
        covered_by: "quasiquote holes ARE evaluated",
    },
    Shape {
        name: "lambda-list-keyword",
        heads: tatara_lisp::binding_shapes::LAMBDA_LIST_KEYWORDS,
        prescription: Prescription::Data,
        example: "(defmacro m (x &rest ys) ...)",
        note: "A marker inside a parameter list, never a value reference.",
        covered_by: "&rest in a macro parameter list is not a reference",
    },
    Shape {
        name: "require-program",
        heads: &["require"],
        prescription: Prescription::Abstain,
        example: "(require \"m\")",
        note: "A program with any `require` is skipped WHOLESALE: imported bindings are invisible here, so every reference to them would report. Abstaining is the honest answer; a partial guard would read like a working one.",
        covered_by: "a program using require abstains wholesale",
    },
    Shape {
        name: "macro-binding-form",
        heads: &[],
        prescription: Prescription::ResolvedByCallerExpansion,
        example: "(dolist (entry xs) ...) / (when-let (f e) ...) / (with-gensyms (loop) ...)",
        note: "RESOLVED BY THE CALLER, not by this crate. These forms bind, but which forms bind is decided by a `defmacro` — unknowable syntactically. `tatara-script lint` therefore macro-EXPANDS (Interpreter::fully_expand, registering macros with ZERO evaluation) before calling `lint_forms`, and the shape disappears. Measured: that alone took tatara-lisp 26 -> 0. A caller using `lint_source` on raw text still sees these; the characterization case pins that.",
        covered_by: "RULE-ALONE macro-binding-form: dolist reports without caller expansion",
    },
    Shape {
        name: "macro-dsl-data",
        heads: &[],
        prescription: Prescription::ResolvedByCallerExpansion,
        example: "(defreversal decouple :losses nothing)",
        note: "RESOLVED BY THE CALLER, same mechanism as macro-binding-form: once the macro is expanded, its keyword-argument constants are no longer read as references. Only holds when the defmacro is IN THE FILE; a DSL supplied by a driver is the non-self-contained row below.",
        covered_by: "RULE-ALONE macro-dsl-data: a DSL constant reports without caller expansion",
    },
    Shape {
        name: "non-self-contained-file",
        heads: &[],
        prescription: Prescription::Abstain,
        example: "substrate's aferir.test.tlisp (concatenated before running); nix's pkgs/tebiki/runbooks/*.tlisp (DSL macros supplied by the driver, zero defmacro in-file)",
        note: "The ONE class expansion cannot reach: the file genuinely does not bind its own names, so nothing is there to register. Reports are correct about the FILE and useless about the PROGRAM. Handled by `;; lint:allow-file unbound-symbol <reason>` in the header (first 20 lines) — one honest line instead of dozens of per-line comments that rot. Exactly two files in the fleet needed it.",
        covered_by: "file-wide suppression silences a non-self-contained file",
    },
];

impl Prescription {
    /// Stable operator-facing label. Derived here so every surface that shows a
    /// prescription — the `--shapes` listing, a future doc generator — spells it
    /// the same way, instead of each caller inventing its own wording.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Prescription::Application => "application  (head + args are references)",
            Prescription::Binds => "binds        (names tracked; never report)",
            Prescription::Data => "data         (not walked)",
            Prescription::DataWithHoles => "data+holes   (only unquoted parts walked)",
            Prescription::Abstain => "abstain      (whole program skipped)",
            Prescription::ResolvedByCallerExpansion => {
                "expand-first (caller macro-expands; rule alone reports)"
            }
            Prescription::UnresolvedFalsePositive => "UNRESOLVED   (false positives today)",
        }
    }
}

/// Operator-facing listing of the shape catalog.
///
/// A `Display` newtype, not a `String`-building function: TYPED EMISSION bans
/// `format!()` for emitted text, and a `Display`-family `write!()` is the
/// sanctioned surface. (This crate had ZERO `format!` before; the first draft of
/// this listing reintroduced eight and had to be rewritten.) It also streams
/// straight into any formatter — stdout, or a test's `to_string()` — with no
/// per-row allocation.
///
/// GENERATED, never hand-maintained: `tatara-script lint --shapes` prints this,
/// and `catalog_listing_shows_every_row` asserts every row appears, so documented
/// coverage cannot diverge from implemented coverage. This is the reflection half
/// of the catalog; the coherence tests are the enforcement half.
pub struct CatalogListing;

impl fmt::Display for CatalogListing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "unbound-symbol — form shapes and their prescribed handling"
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "Each row is what the rule DOES with a shape. Rows marked UNRESOLVED"
        )?;
        writeln!(
            f,
            "produce false positives today and say what would fix them; that is"
        )?;
        writeln!(
            f,
            "why the rule is opt-in (`--unbound`) rather than default-on."
        )?;
        writeln!(f)?;
        for shape in SHAPES {
            writeln!(f, "  {}", shape.name)?;
            writeln!(f, "    prescription : {}", shape.prescription.label())?;
            if !shape.heads.is_empty() {
                writeln!(f, "    heads        : {}", shape.heads.join(" "))?;
            }
            writeln!(f, "    example      : {}", shape.example)?;
            writeln!(f, "    note         : {}", shape.note)?;
            writeln!(f, "    proven by    : {}", shape.covered_by)?;
            writeln!(f)?;
        }
        let unresolved = SHAPES
            .iter()
            .filter(|s| s.prescription == Prescription::UnresolvedFalsePositive)
            .count();
        writeln!(
            f,
            "{} shapes catalogued; {unresolved} still unresolved.",
            SHAPES.len()
        )
    }
}

/// Heads the walker special-cases, drawn from the same consts the match arms
/// use — so the catalog cannot drift from the implementation.
#[must_use]
pub fn special_cased_heads() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    v.extend(DEFINE_HEADS);
    v.extend(LAMBDA_HEADS);
    v.extend(LET_HEADS);
    v.extend(QUOTE_HEADS);
    v
}

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
    if head_symbol(items).is_some_and(|h| h.starts_with(DEF_PREFIX)) {
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
    /// Build the violation text. `String::from` + `push_str`, matching this
    /// crate's existing rule — TYPED EMISSION bans `format!()` for emitted
    /// strings, and the crate had zero `format!` before this rule.
    fn message(name: &str) -> String {
        let mut m = String::from("`");
        m.push_str(name);
        m.push_str(
            "` is not bound by the interpreter or this program. It parses fine and fails only \
             when evaluation reaches it — check the spelling against the installed primitives \
             (e.g. `string-lowercase`, not `string-downcase`), or define it. Suppress with \
             `;; lint:allow unbound-symbol <reason>`.",
        );
        m
    }

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
            Some(h) if QUOTE_HEADS.contains(&h) => {}

            Some(h) if DEFINE_HEADS.contains(&h) => {
                // The bound name belongs to the ENCLOSING scope; parameters to a
                // fresh inner one. `define`'s third item is a VALUE, never a
                // parameter list — which is exactly why it cannot share the
                // generic `def*` arm below.
                let mut names = binder_names(items);
                let params: Vec<String> = if names.len() > 1 {
                    names.split_off(1)
                } else {
                    vec![]
                };
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
            Some(head) if head.starts_with(DEF_PREFIX) => {
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
            Some(h) if LAMBDA_HEADS.contains(&h) => {
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

            Some(kind) if LET_HEADS.contains(&kind) => {
                let pairs = items.get(1).map(let_binding_pairs).unwrap_or_default();
                // THREE genuinely different scoping rules — collapsing `let*`
                // and `letrec` together was a real false positive, not a safe
                // approximation. An earlier version of this comment claimed the
                // incremental walk "can only report fewer, never more"; the
                // canonical `(letrec ((f (fn () (f)))) (f))` disproved it, and
                // only a test found that, which is why the claim now has one.
                match kind {
                    // `let`: initialisers are evaluated in the OUTER scope, so a
                    // self-reference like `(let ((a a)) …)` correctly reports.
                    "let" => {
                        for (_, init) in &pairs {
                            if let Some(v) = init {
                                self.walk(v, scopes, out);
                            }
                        }
                        scopes.push(pairs.into_iter().map(|(n, _)| n).collect());
                    }
                    // `letrec`: EVERY name is visible in EVERY initialiser,
                    // including its own — that is the whole point of the form
                    // (mutual and self recursion). Bind all, then walk.
                    "letrec" => {
                        scopes.push(pairs.iter().map(|(n, _)| n.clone()).collect());
                        for (_, init) in &pairs {
                            if let Some(v) = init {
                                self.walk(v, scopes, out);
                            }
                        }
                    }
                    // `let*`: each initialiser sees the names bound BEFORE it.
                    _ => {
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
                    message: Self::message(&name),
                }
            })
            .collect()
    }
}
