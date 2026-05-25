//! The tatara-script stdlib. Each module registers a family of FFI
//! primitives on `Interpreter<ScriptCtx>`; `install_stdlib` is the
//! single entry point — call after `Interpreter::new()`.
//!
//! `install_stdlib` also calls `tatara_lisp_eval::install_primitives`,
//! `install_hof`, and `install_map` up front so arithmetic, comparison,
//! list primitives (`+`, `-`, `=`, `car`, `cdr`, `cons`, `list`, …) AND
//! higher-order list ops (`map`, `filter`, `reduce`, `foldl`, `foldr`,
//! `for-each`, `apply`, `find`, `every?`, `any?`, `partition`,
//! `group-by`, `sort-by`, `iterate`, `take-while`, `drop-while`,
//! `remove`, `count-if`, `find-index`, `scan-left`, `repeatedly`,
//! `some`) and `(map …)` constructors / accessors are available to any
//! script. Without these, `Interpreter::new()` is intentionally bare —
//! the evaluator leaves primitive registration to the embedder, and
//! historically `install_stdlib` here only wired primitives so HOFs
//! were silently absent at script runtime.
//!
//! Anything beyond these (Lisp-source stdlib like `compose`, `pipe`,
//! `->`, `->>`, `defflow`, `dotimes`, `distinct`, `group-by` helpers)
//! lives in `tatara-lisp-eval::install_lisp_stdlib_with` and requires a
//! host context — it's deliberately NOT installed here today since
//! tatara-script's per-script ScriptCtx isn't available at
//! install-time. Call `install_lisp_stdlib_with` from your binary if
//! you need it.

use tatara_lisp_eval::{
    install_hof, install_lisp_stdlib_with, install_map, install_primitives, Interpreter,
};

use crate::script_ctx::ScriptCtx;

// Core scripting families
pub mod cli;
pub mod crypto_extra;
pub mod dns;
pub mod encoding;
pub mod env;
pub mod fs;
pub mod hash;
pub mod http;
pub mod http_server;
pub mod io;
pub mod json;
pub mod kube;
pub mod list_ext;
pub mod log;
pub mod module;
pub mod os;
pub mod process;
pub mod regex;
pub mod sops;
pub mod string;
pub mod string_ext;
pub mod time;
pub mod toml;
pub mod uuid;
pub mod yaml;

/// Install every stdlib family. Three layers, in order:
///
///   1. Rust primitives (`install_primitives`): arithmetic, comparison,
///      list/string/IO — `+`, `=`, `car`, `cons`, `length`, `reverse`,
///      `string-format`, `print-line`, `read-file`, …
///   2. Higher-order Rust primitives (`install_hof`): `map`, `filter`,
///      `reduce`, `foldl`, `foldr`, `for-each`, `apply`, `find`,
///      `every?`, `any?`, `partition`, `group-by`, `sort-by`,
///      `iterate`, `take-while`, `drop-while`, `remove`, `count-if`,
///      `find-index`, `scan-left`, `repeatedly`, `some`.
///   3. Typed map/dict primitives (`install_map`): `(map ...)` value
///      constructor + accessors (alist-get, etc.).
///
/// Then the FFI families (cli/fs/http/json/regex/sops/...).
/// Finally, the pure-Lisp stdlib (`install_lisp_stdlib_with`) which adds
/// `compose`, `pipe`, `->`, `->>`, `when-let`, `dotimes`, `dolist`,
/// `defflow`, `inc`, `dec`, `even?`, `odd?`, `first`/`second`/`third`,
/// `range`, `zip`, `interleave`, `flatten`, `distinct`, `max-by`,
/// `min-by`, `partial`, `juxt`, `tap`, `not=`, `some?`, `not-empty?`.
///
/// Scripts see the full Clojure-flavored environment without calling
/// any of these installers themselves.
pub fn install_stdlib(interp: &mut Interpreter<ScriptCtx>, ctx: &mut ScriptCtx) {
    install_primitives(interp);
    install_hof(interp);
    install_map(interp);
    cli::install(interp);
    crypto_extra::install(interp);
    dns::install(interp);
    encoding::install(interp);
    env::install(interp);
    fs::install(interp);
    hash::install(interp);
    http::install(interp);
    http_server::install(interp);
    io::install(interp);
    kube::install(interp);
    json::install(interp);
    list_ext::install(interp);
    log::install(interp);
    module::install(interp);
    os::install(interp);
    process::install(interp);
    regex::install(interp);
    sops::install(interp);
    string::install(interp);
    string_ext::install(interp);
    time::install(interp);
    toml::install(interp);
    uuid::install(interp);
    yaml::install(interp);
    // Pure-Lisp layer LAST — it depends on every Rust primitive above
    // (compose calls foldr, juxt calls map, threading macros use list?,
    // etc.). Loading earlier would error on unbound primitives.
    install_lisp_stdlib_with(interp, ctx);
}

#[cfg(test)]
mod surface_tests {
    //! Surface-area regression tests for `install_stdlib`.
    //!
    //! Every name in this module is something a script writer reasonably
    //! expects to be in scope after `install_stdlib`. If one of these
    //! tests fails, it means a previously-claimed primitive is gone — a
    //! breaking change for every consumer .tlisp file in the org.
    //!
    //! Categories covered:
    //!   1. Core Rust primitives (`+`, `=`, `car`, `cons`, `length`, `modulo`, `abs`, `min`, `max`, …)
    //!   2. Higher-order Rust primitives (`map`, `filter`, `reduce`, `foldl`, `foldr`, `for-each`, `apply`, `find`, `every?`, `any?`, `partition`, `group-by`, `sort-by`, `iterate`, `take-while`, `drop-while`, `remove`)
    //!   3. Pure-Lisp stdlib: identity / comp / pipe / partial / juxt / tap, `->` / `->>` threading, `when-let` / `if-let` / `dotimes` / `dolist`, sequence helpers (first/second/third/rest/last/butlast, range, repeat-list, concat, member?, position, zip, interleave, intersperse, flatten, distinct, max-by, min-by), numeric helpers (inc, dec, zero?, positive?, negative?, even?, odd?), predicates (not=, some?, not-empty?)
    //!   4. Clojure-flavored aliases (`fn`, `true`, `false`, `mod`, `rem`, `nil?`, `==`, `next`).

    use crate::{eval_str, Value};

    /// Helper: eval and assert against a stringified result.
    fn eval_eq(src: &str, expected: &str) {
        let v = eval_str(src).expect(src);
        assert_eq!(format!("{v:?}"), expected, "src: {src}");
    }

    fn eval_ok(src: &str) -> Value {
        eval_str(src).unwrap_or_else(|e| panic!("eval failed for {src:?}: {e}"))
    }

    // ── Core primitives ─────────────────────────────────────────────

    #[test]
    fn arith_modulo_and_friends() {
        // The `mod`/`rem` aliases must agree with the canonical `modulo`.
        eval_eq("(modulo 7 3)", "Int(1)");
        eval_eq("(mod 7 3)", "Int(1)");
        eval_eq("(rem 7 3)", "Int(1)");
    }

    #[test]
    fn arith_min_max_abs() {
        eval_eq("(min 1 2 3)", "Int(1)");
        eval_eq("(max 1 2 3)", "Int(3)");
        eval_eq("(abs -5)", "Int(5)");
    }

    #[test]
    fn list_primitives() {
        eval_eq("(car (list 1 2 3))", "Int(1)");
        eval_eq("(length (list 1 2 3 4))", "Int(4)");
    }

    // ── Higher-order Rust primitives (install_hof) ─────────────────

    #[test]
    fn hof_filter_evens() {
        // The headline regression: filter MUST be bound after install_stdlib.
        let v = eval_ok("(filter (lambda (x) (= 0 (modulo x 2))) (list 1 2 3 4 5))");
        assert_eq!(format!("{v:?}"), "[Int(2), Int(4)]");
    }

    #[test]
    fn hof_map_double() {
        let v = eval_ok("(map (lambda (x) (* x 2)) (list 1 2 3))");
        assert_eq!(format!("{v:?}"), "[Int(2), Int(4), Int(6)]");
    }

    #[test]
    fn hof_foldl_sum() {
        eval_eq("(foldl + 0 (list 1 2 3 4 5))", "Int(15)");
    }

    #[test]
    fn hof_foldr_sum() {
        eval_eq("(foldr + 0 (list 1 2 3 4 5))", "Int(15)");
    }

    #[test]
    fn hof_reduce_sum() {
        eval_eq("(reduce + (list 1 2 3 4 5))", "Int(15)");
    }

    #[test]
    fn hof_find_first_match() {
        eval_eq(
            "(find (lambda (x) (> x 2)) (list 1 2 3 4))",
            "Int(3)",
        );
    }

    #[test]
    fn hof_every_and_any() {
        eval_eq("(every? (lambda (x) (> x 0)) (list 1 2 3))", "Bool(true)");
        eval_eq("(any? (lambda (x) (> x 5)) (list 1 2 3))", "Bool(false)");
    }

    #[test]
    fn hof_remove_inverse_of_filter() {
        let v = eval_ok("(remove (lambda (x) (= 0 (modulo x 2))) (list 1 2 3 4 5))");
        assert_eq!(format!("{v:?}"), "[Int(1), Int(3), Int(5)]");
    }

    #[test]
    fn hof_apply() {
        eval_eq("(apply + (list 1 2 3 4))", "Int(10)");
    }

    // ── Pure-Lisp stdlib (install_lisp_stdlib_with) ────────────────

    #[test]
    fn lisp_compose_and_pipe() {
        eval_eq("((compose inc inc) 5)", "Int(7)");
        eval_eq("((pipe inc inc inc) 5)", "Int(8)");
    }

    #[test]
    fn lisp_seq_helpers() {
        eval_eq("(first (list 10 20 30))", "Int(10)");
        eval_eq("(last (list 10 20 30))", "Int(30)");
        let v = eval_ok("(rest (list 10 20 30))");
        assert_eq!(format!("{v:?}"), "[Int(20), Int(30)]");
    }

    #[test]
    fn lisp_range() {
        let v = eval_ok("(range 5)");
        assert_eq!(format!("{v:?}"), "[Int(0), Int(1), Int(2), Int(3), Int(4)]");
    }

    #[test]
    fn lisp_concat_and_distinct() {
        let cat = eval_ok("(concat (list 1 2) (list 3 4))");
        assert_eq!(format!("{cat:?}"), "[Int(1), Int(2), Int(3), Int(4)]");
        let uniq = eval_ok("(distinct (list 1 2 1 3 2 4))");
        assert_eq!(format!("{uniq:?}"), "[Int(1), Int(2), Int(3), Int(4)]");
    }

    #[test]
    fn lisp_numeric_helpers() {
        eval_eq("(inc 5)", "Int(6)");
        eval_eq("(dec 5)", "Int(4)");
        eval_eq("(even? 4)", "Bool(true)");
        eval_eq("(odd? 3)", "Bool(true)");
        eval_eq("(zero? 0)", "Bool(true)");
        eval_eq("(positive? 1)", "Bool(true)");
        eval_eq("(negative? -1)", "Bool(true)");
    }

    #[test]
    fn lisp_threading_macros() {
        // `(-> 5 inc inc inc)` ≡ `(inc (inc (inc 5)))`
        eval_eq("(-> 5 inc inc inc)", "Int(8)");
        // `(->> 5 inc inc)` puts the value in the LAST position. inc is
        // unary so position doesn't matter — confirms the macro fires.
        eval_eq("(->> 5 inc inc)", "Int(7)");
    }

    // ── Clojure aliases ────────────────────────────────────────────

    #[test]
    fn alias_fn_for_lambda() {
        eval_eq("((fn (x) (* x x)) 4)", "Int(16)");
    }

    #[test]
    fn alias_true_false() {
        eval_eq("true", "Bool(true)");
        eval_eq("false", "Bool(false)");
    }

    #[test]
    fn alias_nil_predicate() {
        eval_eq("(nil? (list))", "Bool(true)");
        eval_eq("(nil? (list 1))", "Bool(false)");
    }

    #[test]
    fn alias_double_equals() {
        eval_eq("(== 1 1)", "Bool(true)");
        eval_eq("(== 1 2)", "Bool(false)");
    }

    #[test]
    fn alias_next_is_rest() {
        let v = eval_ok("(next (list 1 2 3))");
        assert_eq!(format!("{v:?}"), "[Int(2), Int(3)]");
    }

    // ── End-to-end script-style smoke ───────────────────────────────

    #[test]
    fn end_to_end_small_pipeline() {
        // Idiomatic "filter even, double, sum" — the smallest realistic
        // shape that exercises filter + map + foldl/reduce in one breath.
        eval_eq(
            "(reduce + (map (fn (x) (* x 2)) (filter even? (range 10))))",
            "Int(40)", // 2+4+6+8 = 20, doubled = 40
        );
    }

    #[test]
    fn end_to_end_threading_with_aliases() {
        eval_eq(
            "(-> 1 inc inc inc inc)",
            "Int(5)",
        );
    }
}
