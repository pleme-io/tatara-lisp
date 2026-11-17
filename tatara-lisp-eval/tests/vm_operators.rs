use tatara_lisp_eval::{
    install_full_stdlib_with, read_spanned, Arity, Env, EvalError, Interpreter, Value,
};

fn fresh() -> Interpreter<()> {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i
}

fn walker(src: &str) -> Result<Value, EvalError> {
    fresh().eval_program(&read_spanned(src).expect("reads"), &mut ())
}

fn vm(src: &str) -> Result<Value, EvalError> {
    fresh().eval_program_vm(&read_spanned(src).expect("reads"), &mut ())
}

fn outcome(r: Result<Value, EvalError>) -> String {
    match r {
        Ok(v) => format!("ok {v}"),
        Err(e) => format!("err {}", e.short_message()),
    }
}

fn agree(src: &str) -> String {
    let w = outcome(walker(src));
    let v = outcome(vm(src));
    assert_eq!(w, v, "walker and vm disagree on {src}");
    v
}

const OPERATORS: [&str; 8] = ["+", "-", "*", "<", ">", "<=", ">=", "="];

#[test]
fn every_operator_agrees_with_the_walker_on_a_seeded_corpus() {
    let mut state: u64 = 0x5eed_0000_0000_00b1;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let edges = [i64::MAX, i64::MIN, 0, 1, -1, i64::MAX - 1, i64::MIN + 1];
    let mut operand = || -> String {
        let r = next();
        match r % 5 {
            0 => edges[(r >> 8) as usize % edges.len()].to_string(),
            1 => format!(
                "{:.3}",
                f64::from(u32::try_from(r >> 32).expect("32 bits")) / 1e3 - 4.0e6
            ),
            2 => "2.0".to_string(),
            _ => i64::from_le_bytes(r.to_le_bytes()).to_string(),
        }
    };
    let mut overflowed = 0;
    let mut cases = 0;
    for _ in 0..100 {
        for op in OPERATORS {
            let src = format!("({op} {} {})", operand(), operand());
            if agree(&src).contains("overflow") {
                overflowed += 1;
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 800);
    assert!(overflowed > 0, "the corpus never reached an overflow edge");
}

#[test]
fn an_operator_a_program_rebinds_is_the_program_s() {
    assert_eq!(agree("(define (+ a b) (- a b)) (+ 5 3)"), "ok 2");
    assert_eq!(agree("(define < (lambda (a b) 'mine)) (< 1 2)"), "ok mine");
}

#[test]
fn an_operator_the_embedder_replaces_is_the_embedder_s() {
    let mut i = fresh();
    i.register_fn("*", Arity::AtLeast(0), |_: &[Value], (): &mut (), _| {
        Ok(Value::Int(-7))
    });
    let forms = read_spanned("(* 6 7)").expect("reads");
    let got = i.eval_program_vm(&forms, &mut ()).expect("runs");
    assert!(matches!(got, Value::Int(-7)), "{got:?}");
}

#[test]
fn a_fork_that_replaces_an_operator_leaves_its_parent_alone() {
    let parent = fresh();
    let mut child = parent.fork();
    child.register_fn("-", Arity::AtLeast(0), |_: &[Value], (): &mut (), _| {
        Ok(Value::Int(99))
    });
    let forms = read_spanned("(- 10 4)").expect("reads");
    let mut parent = parent;
    assert!(matches!(
        child.eval_program_vm(&forms, &mut ()),
        Ok(Value::Int(99))
    ));
    assert!(matches!(
        parent.eval_program_vm(&forms, &mut ()),
        Ok(Value::Int(6))
    ));
}

#[test]
fn a_global_written_after_it_was_read_is_read_again() {
    assert_eq!(
        agree("(define x 1) (define (f) x) (define a (f)) (define x 2) (list a (f))"),
        "ok (1 2)"
    );
    assert_eq!(
        agree(
            "(define n 0) \
             (define (bump k) (if (= k 0) n (begin (set! n (+ n 1)) (bump (- k 1))))) \
             (bump 5)"
        ),
        "ok 5"
    );
    assert_eq!(
        agree("(define (g) (h)) (define (h) 1) (define a (g)) (define (h) 2) (list a (g))"),
        "ok (1 2)"
    );
}

#[test]
fn operators_in_tail_position_and_on_mixed_operands_agree() {
    assert_eq!(agree("(define (f n) (- n 1)) (f 10)"), "ok 9");
    agree("(list (+ 1 2.5) (< 1 2.5) (= 2 2.0) (* 2.0 3.0) (- 7 10) (> 0.5 0.25))");
    agree("(+ 1 \"a\")");
    agree("(< 'a 1)");
}

#[test]
fn a_stamp_moves_on_every_write_and_never_returns() {
    let env = Env::new();
    let s0 = env.stamp();
    assert_eq!(s0, env.stamp());
    env.define("x", Value::Int(1));
    let s1 = env.stamp();
    assert_ne!(s0, s1);
    assert!(env.set("x", Value::Int(1)));
    let s2 = env.stamp();
    assert_ne!(s1, s2);
    assert_ne!(s0, s2);
    assert!(!env.set("absent", Value::Int(1)));
    assert_eq!(s2, env.stamp());
    assert_ne!(Env::new().stamp(), Env::new().stamp());
    let mut deeper = env.clone();
    deeper.push();
    assert_ne!(deeper.stamp(), env.stamp());
}

#[test]
fn a_stamp_covers_eight_frames_and_declines_a_ninth() {
    let mut env = Env::new();
    while env.frame_depth() < 8 {
        env.push();
    }
    assert!(env.stamp().is_some());
    env.push();
    assert!(env.stamp().is_none());
}

#[test]
fn a_compiled_function_called_with_the_wrong_count_is_refused() {
    for src in [
        "(define (f a) a) (f 1 2)",
        "(define (f a b) a) (f 1)",
        "(define (f a) a) (define (g) (f 1 2)) (g)",
    ] {
        assert!(vm(src).is_err(), "the vm ran {src}");
        assert!(walker(src).is_err(), "the walker ran {src}");
    }
    assert_eq!(
        agree("(define (f a &rest more) more) (f 1 2 3)"),
        "ok (2 3)"
    );
    assert!(vm("(define (f a b &rest more) more) (f 1)").is_err());
    agree("(define (f a b &rest more) (list a b more)) (f 1 2)");
}

#[test]
fn every_operator_agrees_on_equal_and_adjacent_operands() {
    let pairs = [
        "1 1",
        "1 2",
        "2 1",
        "-1 -1",
        "0 0",
        "9223372036854775807 9223372036854775807",
        "-9223372036854775808 -9223372036854775808",
        "9223372036854775806 9223372036854775807",
        "2.0 2.0",
        "1.5 2.5",
        "2.5 1.5",
    ];
    for op in OPERATORS {
        for pair in pairs {
            agree(&format!("({op} {pair})"));
        }
    }
    assert_eq!(
        agree("(list (> 3 3) (>= 3 3) (< 3 3) (<= 3 3) (= 3 3))"),
        "ok (#f #t #f #t #t)"
    );
}

#[test]
fn a_tail_call_on_the_vm_runs_in_constant_frames() {
    let mut i = fresh();
    i.set_budget(tatara_lisp_eval::vm::Budget {
        fuel: None,
        max_depth: Some(50),
        quantum: None,
    })
    .expect("a depth bound");
    let forms = read_spanned(
        "(define (down n) (if (= n 0) :done (down (- n 1)))) \
         (define (hop n) (skip n)) (define (skip n) (if (= n 0) :done (hop (- n 1)))) \
         (list (down 100000) (hop 100000))",
    )
    .expect("reads");
    let got = i
        .eval_program_vm(&forms, &mut ())
        .expect("runs within 50 frames");
    assert_eq!(format!("{got}"), "(:done :done)");
}

fn counted_equality(i: &mut Interpreter<()>) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for (name, negate) in [("equal?", false), ("not=", true)] {
        let seen = std::sync::Arc::clone(&calls);
        i.register_fn(name, Arity::Exact(2), move |a: &[Value], (): &mut (), _| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let same = match (&a[0], &a[1]) {
                (Value::Int(x), Value::Int(y)) => x == y,
                (Value::Float(x), Value::Float(y)) => x == y,
                (Value::Str(x), Value::Str(y)) => x == y,
                _ => false,
            };
            Ok(Value::Bool(same != negate))
        });
    }
    calls
}

#[test]
fn an_adopted_equality_runs_in_place_on_numbers_and_calls_its_function_otherwise() {
    let mut i = fresh();
    let calls = counted_equality(&mut i);
    assert!(i.adopt_operator("equal?"));
    assert!(i.adopt_operator("not="));
    let forms = read_spanned(
        "(define (count n acc) (if (equal? n 0) acc (count (- n 1) (+ acc 1)))) \
         (list (count 1000 0) (not= 1.5 2.5) (equal? 2.0 2.0) (equal? \"a\" \"a\") (not= \"a\" \"b\"))",
    )
    .expect("reads");
    let got = i.eval_program_vm(&forms, &mut ()).expect("runs");
    assert_eq!(got.to_string(), "(1000 #t #t #t #t)");
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[test]
fn equality_nobody_adopted_and_equality_registered_again_are_called() {
    let mut i = fresh();
    let calls = counted_equality(&mut i);
    let forms = read_spanned("(list (equal? 1 1) (not= 1 2))").expect("reads");
    i.eval_program_vm(&forms, &mut ()).expect("runs");
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert!(i.adopt_operator("equal?"));
    let again = counted_equality(&mut i);
    let got = i.eval_program_vm(&forms, &mut ()).expect("runs");
    assert_eq!(got.to_string(), "(#t #t)");
    assert_eq!(again.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert!(!i.adopt_operator("length"));
}

#[test]
fn adopted_equality_agrees_with_the_walker_on_a_seeded_corpus() {
    let mut state: u64 = 0x5eed_0000_0000_e0e0;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let mut operand = || -> String {
        let r = next();
        match r % 6 {
            0 => ["0", "1", "-1", "0.0", "-0.0", "1.0"][(r >> 8) as usize % 6].to_string(),
            1 => format!(
                "{:.1}",
                f64::from(u32::try_from(r >> 40).expect("bits")) / 10.0
            ),
            2 => "\"x\"".to_string(),
            _ => (i64::from_le_bytes(r.to_le_bytes()) % 4).to_string(),
        }
    };
    for _ in 0..300 {
        for op in ["equal?", "not="] {
            let src = format!("({op} {} {})", operand(), operand());
            let mut w = fresh();
            counted_equality(&mut w);
            let mut v = fresh();
            counted_equality(&mut v);
            assert!(v.adopt_operator(op));
            let forms = read_spanned(&src).expect("reads");
            assert_eq!(
                outcome(w.eval_program(&forms, &mut ())),
                outcome(v.eval_program_vm(&forms, &mut ())),
                "{src}"
            );
        }
    }
}
