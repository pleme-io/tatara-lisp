//! Pure-Lisp standard library — installed after the Rust primitives and
//! the higher-order Rust primitives (hof.rs) are registered. Defined as
//! tatara-lisp source embedded with `include_str!`.
//!
//! Layering principle: anything that can be expressed naturally in Lisp
//! lives here, not in Rust. The Rust primitive layer is the minimum
//! semantic floor — everything compositional / surface-syntax-flavored
//! stays in tlisp where it can be edited without recompiling.
//!
//! What ships:
//!
//! ```text
//! ;; identity / flip / const
//!   (identity x), (const x), (flip f)
//!
//! ;; composition
//!   (comp f g)            ; binary composition: (comp f g) x = f(g x)
//!   (compose &rest fs)    ; variadic right-to-left composition
//!   (pipe &rest fs)       ; variadic left-to-right composition
//!
//! ;; partial / juxt
//!   (partial f &rest more)   ; right-extended partial
//!   (juxt &rest fs)          ; (juxt f g h) x → (list (f x) (g x) (h x))
//!
//! ;; tap / doto
//!   (tap f x)             ; runs (f x) for side effect, returns x
//!
//! ;; threading macros (Clojure-style)
//!   (-> x f1 f2 f3)       ; left-to-right thread; f's applied as first arg
//!   (->> x f1 f2 f3)      ; left-to-right thread; f's applied as LAST arg
//!
//! ;; control flow macros
//!   (when-let (name expr) body...)    ; bind; if truthy run body
//!
//! ;; loop macros
//!   (dotimes (i n) body...)           ; i ranges 0..n
//!   (dolist (x xs) body...)           ; iterate xs
//!
//! ;; composition macros (program flow)
//!   (defflow name f1 f2 f3 ...)       ; (define name (compose f3 f2 f1))
//!
//! ;; sequence helpers
//!   first/second/third, rest, last, butlast, empty?, not-empty?
//!   range, repeat-list, concat
//!   member?, position
//!   zip, interleave, intersperse, flatten, distinct, max-by, min-by
//!
//! ;; numeric helpers
//!   inc, dec, zero?, positive?, negative?, even?, odd?
//!
//! ;; Predicates
//!   not=, some?
//! ```

use crate::eval::Interpreter;

/// The full Lisp-side standard library, parsed and evaluated at install time.
pub const STDLIB_SOURCE: &str = include_str!("lisp_stdlib.tlisp");

/// Install the pure-Lisp stdlib using a host to drive evaluation. The
/// embedded library is host-state-free; the host is required only to
/// satisfy `eval_program`'s signature.
///
/// Must be called after `install_primitives` and `install_hof`.
///
/// Panics if the embedded source fails to parse or evaluate. The embedded
/// library is part of the binary and is verified by `cargo test`.
pub fn install_lisp_stdlib_with<H: 'static>(interp: &mut Interpreter<H>, host: &mut H) {
    let forms = tatara_lisp::read_spanned(STDLIB_SOURCE)
        .expect("embedded tatara-lisp stdlib failed to parse");
    interp
        .eval_program(&forms, host)
        .expect("embedded tatara-lisp stdlib failed to evaluate");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{install_full_stdlib_with, Value};
    use tatara_lisp::read_spanned;

    struct NoHost;

    fn run(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    // ── identity / const / flip ───────────────────────────────────

    #[test]
    fn identity_returns_arg() {
        assert!(matches!(run("(identity 42)"), Value::Int(42)));
    }

    #[test]
    fn const_ignores_args() {
        assert!(matches!(run("((const 7) 1 2 3)"), Value::Int(7)));
    }

    #[test]
    fn flip_swaps_two_args() {
        // flip on `-`: (flip-) 1 2 = 2 - 1 = 1
        assert!(matches!(run("((flip -) 1 2)"), Value::Int(1)));
    }

    // ── compose / pipe ────────────────────────────────────────────

    #[test]
    fn comp_binary() {
        // (comp inc inc) 5 → (inc (inc 5)) → 7
        assert!(matches!(run("((comp inc inc) 5)"), Value::Int(7)));
    }

    #[test]
    fn compose_variadic_right_to_left() {
        // (compose inc inc inc) 0 → 3
        assert!(matches!(run("((compose inc inc inc) 0)"), Value::Int(3)));
    }

    #[test]
    fn pipe_variadic_left_to_right() {
        // (pipe inc dec) 5 → (dec (inc 5)) = 5
        assert!(matches!(run("((pipe inc dec) 5)"), Value::Int(5)));
    }

    #[test]
    fn pipe_three_funcs() {
        // (pipe inc (lambda (x) (* x 2)) inc) 1 = 5
        // step 0: 1
        // pipe: foldl (flip comp) identity (inc, double, inc)
        //   = (compose inc (compose double (compose inc identity)))
        //   = inc ∘ double ∘ inc
        // applied to 1: inc(1)=2, double(2)=4, inc(4)=5
        assert!(matches!(
            run("((pipe inc (lambda (x) (* x 2)) inc) 1)"),
            Value::Int(5)
        ));
    }

    // ── partial / juxt ────────────────────────────────────────────

    #[test]
    fn partial_left_binds() {
        // (partial + 10) 5 → 15
        assert!(matches!(run("((partial + 10) 5)"), Value::Int(15)));
    }

    #[test]
    fn juxt_runs_all_in_parallel() {
        // (juxt inc dec) 10 → (11 9)
        let v = run("((juxt inc dec) 10)");
        assert_eq!(format!("{v}"), "(11 9)");
    }

    // ── tap ───────────────────────────────────────────────────────

    #[test]
    fn tap_returns_unchanged() {
        // identity-as-tap-function still returns the value.
        assert!(matches!(run("(tap identity 42)"), Value::Int(42)));
    }

    // ── threading macros ──────────────────────────────────────────

    #[test]
    fn arrow_threads_first() {
        // (-> 5 inc inc inc) → 8
        assert!(matches!(run("(-> 5 inc inc inc)"), Value::Int(8)));
    }

    #[test]
    fn arrow_with_call_form_threads_first_position() {
        // (-> 10 (- 3) (- 2)) → ((10-3)-2) = 5
        assert!(matches!(run("(-> 10 (- 3) (- 2))"), Value::Int(5)));
    }

    #[test]
    fn arrow_arrow_threads_last() {
        // (->> (list 1 2 3) (map inc) (filter even?)) → (2 4)
        let v = run("(->> (list 1 2 3) (map inc) (filter even?))");
        assert_eq!(format!("{v}"), "(2 4)");
    }

    // ── control flow ──────────────────────────────────────────────

    #[test]
    fn when_let_truthy_runs_body() {
        let v = run("(when-let (x 7) (* x x))");
        assert!(matches!(v, Value::Int(49)));
    }

    #[test]
    fn when_let_falsy_returns_nil() {
        let v = run("(when-let (x #f) (* x x))");
        assert!(matches!(v, Value::Nil));
    }

    #[test]
    fn if_let_picks_branch() {
        let v = run("(if-let (x 5) x (- 1))");
        assert!(matches!(v, Value::Int(5)));
        let v = run("(if-let (x #f) x (- 1))");
        assert!(matches!(v, Value::Int(-1)));
    }

    // ── loops ─────────────────────────────────────────────────────

    #[test]
    fn dotimes_iterates() {
        // dotimes returns nil; verify it runs n times by accumulating
        // through a side-effect-free path via define + collect.
        let v = run(
            "(define accum (list))
             (define (push! x) (set! accum (append accum (list x))))
             (dotimes (i 5) (push! i))
             accum",
        );
        assert_eq!(format!("{v}"), "(0 1 2 3 4)");
    }

    #[test]
    fn dolist_iterates() {
        let v = run(
            "(define s 0)
             (define (bump! x) (set! s (+ s x)))
             (dolist (n (list 1 2 3 4 5)) (bump! n))
             s",
        );
        assert!(matches!(v, Value::Int(15)));
    }

    // ── defflow ───────────────────────────────────────────────────

    #[test]
    fn defflow_creates_pipeline() {
        let v = run(
            "(defflow process inc inc inc)
             (process 10)",
        );
        assert!(matches!(v, Value::Int(13)));
    }

    #[test]
    fn defflow_with_multiple_steps() {
        // (defflow shape inc (lambda (x) (* x x)) inc) at 2:
        //   inc(2)=3, sq(3)=9, inc(9)=10
        let v = run(
            "(defflow shape inc (lambda (x) (* x x)) inc)
             (shape 2)",
        );
        assert!(matches!(v, Value::Int(10)));
    }

    // ── seq helpers ───────────────────────────────────────────────

    #[test]
    fn first_rest_second_third() {
        assert!(matches!(run("(first  (list 10 20 30))"), Value::Int(10)));
        assert!(matches!(run("(second (list 10 20 30))"), Value::Int(20)));
        assert!(matches!(run("(third  (list 10 20 30))"), Value::Int(30)));
        let v = run("(rest (list 10 20 30))");
        assert_eq!(format!("{v}"), "(20 30)");
    }

    #[test]
    fn last_butlast_handle_long_lists() {
        assert!(matches!(run("(last (list 1 2 3 4 5))"), Value::Int(5)));
        let v = run("(butlast (list 1 2 3 4 5))");
        assert_eq!(format!("{v}"), "(1 2 3 4)");
    }

    #[test]
    fn empty_predicate() {
        assert!(matches!(run("(empty? (list))"), Value::Bool(true)));
        assert!(matches!(run("(empty? (list 1))"), Value::Bool(false)));
        assert!(matches!(run("(not-empty? (list 1))"), Value::Bool(true)));
    }

    #[test]
    fn range_one_arg() {
        let v = run("(range 5)");
        assert_eq!(format!("{v}"), "(0 1 2 3 4)");
    }

    #[test]
    fn range_two_args() {
        let v = run("(range 2 6)");
        assert_eq!(format!("{v}"), "(2 3 4 5)");
    }

    #[test]
    fn range_three_args_step() {
        let v = run("(range 0 10 2)");
        assert_eq!(format!("{v}"), "(0 2 4 6 8)");
    }

    #[test]
    fn range_negative_step() {
        let v = run("(range 10 0 (- 2))");
        assert_eq!(format!("{v}"), "(10 8 6 4 2)");
    }

    #[test]
    fn repeat_list_generates() {
        let v = run("(repeat-list 7 4)");
        assert_eq!(format!("{v}"), "(7 7 7 7)");
    }

    #[test]
    fn concat_chains_lists() {
        let v = run("(concat (list 1 2) (list 3 4) (list 5))");
        assert_eq!(format!("{v}"), "(1 2 3 4 5)");
    }

    #[test]
    fn member_predicate() {
        assert!(matches!(
            run("(member? 3 (list 1 2 3 4))"),
            Value::Bool(true)
        ));
        assert!(matches!(
            run("(member? 99 (list 1 2 3))"),
            Value::Bool(false)
        ));
    }

    #[test]
    fn position_finds_or_neg_one() {
        assert!(matches!(run("(position 3 (list 1 2 3 4))"), Value::Int(2)));
        assert!(matches!(
            run("(position 99 (list 1 2 3))"),
            Value::Int(-1)
        ));
    }

    #[test]
    fn zip_pairs_two_lists() {
        let v = run("(zip (list 1 2 3) (list \"a\" \"b\" \"c\"))");
        assert_eq!(format!("{v}"), "((1 \"a\") (2 \"b\") (3 \"c\"))");
    }

    #[test]
    fn interleave_two_lists() {
        let v = run("(interleave (list 1 2 3) (list \"a\" \"b\" \"c\"))");
        assert_eq!(format!("{v}"), "(1 \"a\" 2 \"b\" 3 \"c\")");
    }

    #[test]
    fn intersperse_inserts_separator() {
        let v = run("(intersperse 0 (list 1 2 3))");
        assert_eq!(format!("{v}"), "(1 0 2 0 3)");
    }

    #[test]
    fn flatten_recursive() {
        let v = run("(flatten (list 1 (list 2 (list 3 4)) 5))");
        assert_eq!(format!("{v}"), "(1 2 3 4 5)");
    }

    #[test]
    fn distinct_drops_duplicates() {
        let v = run("(distinct (list 1 2 1 3 2 4))");
        assert_eq!(format!("{v}"), "(1 2 3 4)");
    }

    #[test]
    fn max_by_min_by() {
        // longest / shortest by string-length
        assert!(matches!(
            run("(max-by string-length (list \"a\" \"abc\" \"ab\"))"),
            Value::Str(_)
        ));
        let v = run("(max-by string-length (list \"a\" \"abc\" \"ab\"))");
        assert_eq!(format!("{v}"), "\"abc\"");
        let v = run("(min-by string-length (list \"abc\" \"a\" \"ab\"))");
        assert_eq!(format!("{v}"), "\"a\"");
    }

    // ── numeric helpers ───────────────────────────────────────────

    #[test]
    fn inc_dec_zero_pos_neg() {
        assert!(matches!(run("(inc 5)"), Value::Int(6)));
        assert!(matches!(run("(dec 5)"), Value::Int(4)));
        assert!(matches!(run("(zero? 0)"), Value::Bool(true)));
        assert!(matches!(run("(positive? 5)"), Value::Bool(true)));
        assert!(matches!(run("(negative? (- 5))"), Value::Bool(true)));
        assert!(matches!(run("(even? 4)"), Value::Bool(true)));
        assert!(matches!(run("(odd? 5)"), Value::Bool(true)));
    }

    // ── general predicates ────────────────────────────────────────

    #[test]
    fn not_eq_and_some() {
        assert!(matches!(run("(not= 1 2)"), Value::Bool(true)));
        assert!(matches!(run("(not= 1 1)"), Value::Bool(false)));
        assert!(matches!(run("(some? 5)"), Value::Bool(true)));
        assert!(matches!(run("(some? ())"), Value::Bool(false)));
    }

    // ── compositions: real-world usage ────────────────────────────

    #[test]
    fn map_compose_filter_pipeline() {
        // square → filter even → sum.
        let v = run(
            "(reduce + 0
                     (filter even?
                             (map (lambda (x) (* x x))
                                  (range 1 6))))",
        );
        // Squares of 1..5: 1 4 9 16 25 → evens: 4 16 → sum: 20
        assert!(matches!(v, Value::Int(20)));
    }

    #[test]
    fn threading_macro_with_seq_pipeline() {
        let v = run(
            "(->> (range 1 6)
                  (map (lambda (x) (* x x)))
                  (filter even?)
                  (reduce + 0))",
        );
        assert!(matches!(v, Value::Int(20)));
    }

    #[test]
    fn defflow_used_in_pipeline() {
        let v = run(
            "(defflow process
                inc
                (lambda (x) (* x 2))
                inc)
             (map process (range 1 4))",
        );
        // process(1)=5, process(2)=7, process(3)=9
        assert_eq!(format!("{v}"), "(5 7 9)");
    }

    // ── State machines ─────────────────────────────────────────────

    #[test]
    fn defsm_traffic_light_cycles_through_states() {
        let v = run(
            "(defsm light
               :initial :red
               :transitions
                 (list (list :red    :go    :green)
                       (list :green  :slow  :yellow)
                       (list :yellow :stop  :red)))
             (sm-current light)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "red"));
    }

    #[test]
    fn defsm_send_advances_state() {
        let v = run(
            "(defsm light
               :initial :red
               :transitions
                 (list (list :red    :go    :green)
                       (list :green  :slow  :yellow)
                       (list :yellow :stop  :red)))
             (sm-send light :go)
             (sm-current light)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "green"));
    }

    #[test]
    fn defsm_full_cycle_back_to_red() {
        let v = run(
            "(defsm light
               :initial :red
               :transitions
                 (list (list :red    :go    :green)
                       (list :green  :slow  :yellow)
                       (list :yellow :stop  :red)))
             (sm-send light :go)
             (sm-send light :slow)
             (sm-send light :stop)
             (sm-current light)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "red"));
    }

    #[test]
    fn defsm_no_transition_stays_put() {
        let v = run(
            "(defsm light
               :initial :red
               :transitions
                 (list (list :red    :go    :green)))
             (sm-send light :nonsense)
             (sm-current light)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "red"));
    }

    #[test]
    fn defsm_can_predicate() {
        let v = run(
            "(defsm light
               :initial :red
               :transitions (list (list :red :go :green)))
             (list (sm-can? light :go) (sm-can? light :stop))",
        );
        assert_eq!(format!("{v}"), "(#t #f)");
    }

    #[test]
    fn defsm_history_tracks_visited() {
        let v = run(
            "(defsm light
               :initial :red
               :transitions
                 (list (list :red :go :green)
                       (list :green :slow :yellow)))
             (sm-send light :go)
             (sm-send light :slow)
             (sm-history light)",
        );
        // Newest-first
        assert_eq!(format!("{v}"), "(:yellow :green :red)");
    }

    // ── Actors ─────────────────────────────────────────────────────

    #[test]
    fn actor_processes_messages_one_at_a_time() {
        // Counter actor — increment by message integer.
        let v = run(
            "(defactor c 0 (lambda (state msg) (+ state msg)))
             (actor-tell c 5)
             (actor-tell c 10)
             (actor-step! c)
             (actor-step! c)
             (actor-state c)",
        );
        assert!(matches!(v, Value::Int(15)));
    }

    #[test]
    fn actor_drain_processes_all_messages() {
        let v = run(
            "(defactor c 0 (lambda (state msg) (+ state msg)))
             (actor-tell c 1)
             (actor-tell c 2)
             (actor-tell c 3)
             (actor-tell c 4)
             (actor-drain! c)",
        );
        assert!(matches!(v, Value::Int(10)));
    }

    #[test]
    fn actor_run_processes_n_messages() {
        let v = run(
            "(defactor c 0 (lambda (state msg) (+ state msg)))
             (actor-tell c 1)
             (actor-tell c 2)
             (actor-tell c 3)
             (actor-run! c 2)
             (actor-state c)",
        );
        // Processed first 2: 1+2 = 3
        assert!(matches!(v, Value::Int(3)));
    }

    // ── Observer ───────────────────────────────────────────────────

    #[test]
    fn subject_emits_to_subscribers() {
        // Two subscribers, both record into their own counters.
        let v = run(
            "(define s (make-subject))
             (define a 0)
             (define b 0)
             (subject-subscribe! s (lambda (m) (set! a (+ a m))))
             (subject-subscribe! s (lambda (m) (set! b (* b 10))))
             (subject-emit! s 5)
             (subject-emit! s 3)
             (list a b)",
        );
        // a: 0 → 5 → 8
        // b: 0 → 0 → 0  (0*10*10 = 0)
        assert_eq!(format!("{v}"), "(8 0)");
    }

    // ── Strategy ───────────────────────────────────────────────────

    #[test]
    fn strategy_picks_named_variant() {
        let v = run(
            "(defstrategy formatter
               :json    (lambda (x) (string-append \"j:\" x))
               :default (lambda (x) (string-append \"d:\" x)))
             (list (strategy-call formatter :json    \"hi\")
                   (strategy-call formatter :unknown \"hi\"))",
        );
        assert_eq!(format!("{v}"), "(\"j:hi\" \"d:hi\")");
    }

    // ── Decorator ──────────────────────────────────────────────────

    #[test]
    fn decorator_wraps_with_before_after() {
        let v = run(
            "(define log (list))
             (define (push! x) (set! log (append log (list x))))
             (define wrapped
               (decorate
                 (lambda (n) (* n 2))
                 :before (lambda (n) (push! :before))
                 :after  (lambda (result n) (push! :after))))
             (define result (wrapped 5))
             (list result log)",
        );
        // Result is 10; log is (:before :after)
        assert_eq!(format!("{v}"), "(10 (:before :after))");
    }

    // ── Visitor ────────────────────────────────────────────────────

    #[test]
    fn visitor_dispatches_on_tag() {
        let v = run(
            "(defvisitor render
               :text  (lambda (s) (string-append \"<text>\" s))
               :image (lambda (url) (string-append \"<img \" url \">\")))
             (list (visit render (list :text \"hello\"))
                   (visit render (list :image \"u.png\")))",
        );
        assert_eq!(format!("{v}"), "(\"<text>hello\" \"<img u.png>\")");
    }

    // ── Pipeline ───────────────────────────────────────────────────

    #[test]
    fn pipeline_runs_stages_in_order() {
        let v = run(
            "(define p (make-pipeline
               (list (list :double (lambda (x) (* x 2)))
                     (list :add-one (lambda (x) (+ x 1)))
                     (list :square  (lambda (x) (* x x))))))
             (pipeline-run! p 3)",
        );
        // 3 → double=6 → +1=7 → square=49
        assert!(matches!(v, Value::Int(49)));
    }

    // ── Event store ────────────────────────────────────────────────

    #[test]
    fn event_store_appends_and_projects() {
        let v = run(
            "(define s (make-event-store))
             (event-append! s :+1)
             (event-append! s :+2)
             (event-append! s :+10)
             (event-project s
               (lambda (acc evt)
                 (cond ((equal? evt :+1)  (+ acc 1))
                       ((equal? evt :+2)  (+ acc 2))
                       ((equal? evt :+10) (+ acc 10))
                       (else acc)))
               0)",
        );
        assert!(matches!(v, Value::Int(13)));
    }

    // ── CQRS Bus ───────────────────────────────────────────────────

    #[test]
    fn defcommand_defquery_dispatch() {
        let v = run(
            "(define bus (make-bus))
             (define balance 100)
             (defcommand bus :deposit (n) (set! balance (+ balance n)))
             (defquery   bus :balance ()  balance)
             (dispatch-command bus :deposit 25)
             (dispatch-command bus :deposit 25)
             (dispatch-query   bus :balance)",
        );
        assert!(matches!(v, Value::Int(150)));
    }

    // ── Transducer ─────────────────────────────────────────────────

    #[test]
    fn transducer_runs_mealy_machine() {
        // Even-parity bit detector. State :even / :odd.
        // 0 keeps state; 1 flips state; output the new state.
        let v = run(
            "(define t (make-transducer
               :initial :even
               :type :mealy
               :transitions
                 (list (list :even 0 :even :even)
                       (list :even 1 :odd  :odd)
                       (list :odd  0 :odd  :odd)
                       (list :odd  1 :even :even))))
             (transducer-run! t (list 1 0 1 1 0))",
        );
        // After feeds: 1→odd, 0→odd, 1→even, 1→odd, 0→odd
        assert_eq!(format!("{v}"), "(:odd :odd :even :odd :odd)");
    }

    // ── define-record ─────────────────────────────────────────────

    #[test]
    fn define_record_constructor_and_accessors() {
        let v = run(
            "(define-record point (x y))
             (define p (make-point 3 4))
             (list (point-x p) (point-y p))",
        );
        assert_eq!(format!("{v}"), "(3 4)");
    }

    #[test]
    fn define_record_predicate() {
        let v = run(
            "(define-record point (x y))
             (define p (make-point 1 2))
             (list (point? p) (point? 42) (point? (hash-map :other 1)))",
        );
        assert_eq!(format!("{v}"), "(#t #f #f)");
    }

    #[test]
    fn define_record_setter_returns_new_value() {
        let v = run(
            "(define-record point (x y))
             (define p (make-point 1 2))
             (define p2 (point-set-x p 99))
             (list (point-x p) (point-x p2))",
        );
        // p unchanged, p2 has new x.
        assert_eq!(format!("{v}"), "(1 99)");
    }

    #[test]
    fn define_record_with_three_fields() {
        let v = run(
            "(define-record user (id name email))
             (define u (make-user 7 \"luis\" \"luis@example.com\"))
             (list (user-id u) (user-name u) (user-email u))",
        );
        assert_eq!(format!("{v}"), "(7 \"luis\" \"luis@example.com\")");
    }

    #[test]
    fn define_record_to_map_returns_underlying() {
        let v = run(
            "(define-record point (x y))
             (define p (make-point 1 2))
             (hash-map-get (point->map p) :__type)",
        );
        assert!(matches!(v, Value::Keyword(s) if &*s == "point"));
    }

    // ── Lazy sequences ────────────────────────────────────────────

    #[test]
    fn delay_force_evaluates_once_and_caches() {
        let v = run(
            "(define n 0)
             (define p (delay (begin (set! n (+ n 1)) :computed)))
             (force p)
             (force p)
             (force p)
             n",
        );
        // Even after 3 forces, the body ran exactly once.
        assert!(matches!(v, Value::Int(1)));
    }

    #[test]
    fn promise_predicate() {
        assert!(matches!(run("(promise? (delay 1))"), Value::Bool(true)));
        assert!(matches!(run("(promise? 1)"), Value::Bool(false)));
    }

    #[test]
    fn lazy_take_realizes_finite_prefix() {
        let v = run(
            "(define naturals (iterate-lazy inc 0))
             (lazy-take 5 naturals)",
        );
        assert_eq!(format!("{v}"), "(0 1 2 3 4)");
    }

    #[test]
    fn lazy_filter_drives_through_infinite() {
        // First 3 even naturals.
        let v = run(
            "(define naturals (iterate-lazy inc 0))
             (lazy-take 3 (lazy-filter even? naturals))",
        );
        assert_eq!(format!("{v}"), "(0 2 4)");
    }

    #[test]
    fn lazy_map_transforms() {
        let v = run(
            "(define naturals (iterate-lazy inc 1))
             (lazy-take 4 (lazy-map (lambda (x) (* x x)) naturals))",
        );
        assert_eq!(format!("{v}"), "(1 4 9 16)");
    }

    #[test]
    fn cycle_repeats_finite_list() {
        let v = run("(lazy-take 7 (cycle (list :a :b :c)))");
        assert_eq!(format!("{v}"), "(:a :b :c :a :b :c :a)");
    }

    #[test]
    fn repeat_lazy_infinite_constant() {
        let v = run("(lazy-take 4 (repeat-lazy 7))");
        assert_eq!(format!("{v}"), "(7 7 7 7)");
    }

    #[test]
    fn lazy_drop_skips_prefix() {
        let v = run(
            "(define naturals (iterate-lazy inc 0))
             (lazy-take 3 (lazy-drop 5 naturals))",
        );
        assert_eq!(format!("{v}"), "(5 6 7)");
    }
}
