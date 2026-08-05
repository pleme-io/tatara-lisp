//! Which head symbols INTRODUCE names — the one table every syntactic
//! walker over `Spanned` reads.
//!
//! WHY THIS LIVES IN THE BASE CRATE. Two passes in this workspace walk raw
//! (un-evaluated) source and must know where a name stops meaning what the
//! enclosing scope says it means:
//!
//!   * `tatara_lisp_lint::rules::unbound_symbol` — a reference to a name
//!     nothing binds. Its module docs record the measured cost of getting
//!     this list wrong: a fleet sweep went 422 -> 87 false positives purely
//!     by handling `defmacro`'s three-part shape, the lambda-list keywords,
//!     the generic `def…` prefix and the `fn` / `λ` / `catch` aliases.
//!   * `tatara_lisp_eval::build_check` — argument-count checking. A local
//!     binder that shadows a top-level function name is the ONLY way that
//!     pass can invent an arity error out of correct code
//!     (`(define (twice f x) (f (f x)))` where `f` is also a 2-argument
//!     top-level function), so it needs exactly the same table.
//!
//! Neither crate depends on the other — by design, so the linter cannot keep
//! a stale copy of the interpreter (see `unbound_symbol`'s module docs). That
//! makes `tatara-lisp`, which both already depend on, the only place a shared
//! table can live without inverting the layering. A second copy in the second
//! walker would be a table that is *free to disagree*, and the two walkers
//! disagreeing about what `catch` binds is precisely a false positive.
//!
//! This module is DATA ONLY — no walker, no scope stack. The two consumers
//! have genuinely different jobs (report an unknown name / count arguments)
//! and their walks are not the same function; what they share is the
//! vocabulary, and only the vocabulary is lifted here.

/// `(define NAME v)` / `(define (NAME params…) body…)`.
///
/// Kept separate from [`DEF_PREFIX`] because item 3 is a VALUE, not a
/// parameter list — the two shapes cannot share a walker arm.
pub const DEFINE_HEADS: &[&str] = &["define"];

/// Lambda-shaped: a parameter list (or a bare rest symbol) followed by a
/// body. `fn` / `λ` are aliases; `catch` binds `(e)` exactly the same way.
pub const LAMBDA_HEADS: &[&str] = &["lambda", "fn", "λ", "catch"];

/// `((name init) …)` bindings then a body. `let` evaluates initialisers in
/// the OUTER scope; `let*` / `letrec` see the names bound so far.
pub const LET_HEADS: &[&str] = &["let", "let*", "letrec"];

/// Contents are data, never references.
pub const QUOTE_HEADS: &[&str] = &["quote"];

/// Prefix for every other definition form — `defmacro`, plus driver and
/// user-macro heads (`deftest`, `defphase`, `defreversal`, …). A prefix
/// rather than a list because the set is OPEN: those heads may be defined in
/// a file the walker is not looking at, or by a test driver rather than by
/// the interpreter.
pub const DEF_PREFIX: &str = "def";

/// Markers inside a parameter list, never value references — and never a
/// positional parameter either, which is why an arity checker must treat a
/// signature containing one as VARIADIC rather than counting the symbols.
pub const LAMBDA_LIST_KEYWORDS: &[&str] =
    &["&rest", "&optional", "&body", "&key", "&aux", "&whole"];

/// Does this parameter-list entry mark the end of the fixed positional
/// parameters? Used by an arity checker to decline to claim an arity.
#[must_use]
pub fn is_lambda_list_keyword(name: &str) -> bool {
    LAMBDA_LIST_KEYWORDS.contains(&name)
}

/// Does this head introduce names into a scope its body can see?
///
/// `define` and the `def…` family bind into the ENCLOSING scope as well, so
/// a caller that only needs "should I push a scope here?" gets `true` for
/// all of them; deciding *which* names go where stays with the caller, whose
/// two consumers genuinely differ.
#[must_use]
pub fn is_binding_head(head: &str) -> bool {
    DEFINE_HEADS.contains(&head)
        || LAMBDA_HEADS.contains(&head)
        || LET_HEADS.contains(&head)
        || head.starts_with(DEF_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn define_is_a_binding_head_and_so_is_the_open_def_family() {
        assert!(is_binding_head("define"));
        assert!(is_binding_head("defmacro"));
        assert!(is_binding_head("deftest"));
        assert!(is_binding_head("defreversal"));
    }

    #[test]
    fn lambda_aliases_all_bind() {
        for h in LAMBDA_HEADS {
            assert!(is_binding_head(h), "{h} should bind");
        }
    }

    #[test]
    fn an_ordinary_application_head_binds_nothing() {
        assert!(!is_binding_head("string-append"));
        assert!(!is_binding_head("quote"));
        assert!(!is_binding_head("if"));
    }

    #[test]
    fn lambda_list_keywords_are_recognized() {
        assert!(is_lambda_list_keyword("&rest"));
        assert!(is_lambda_list_keyword("&optional"));
        assert!(!is_lambda_list_keyword("rest"));
        assert!(!is_lambda_list_keyword("x"));
    }
}
