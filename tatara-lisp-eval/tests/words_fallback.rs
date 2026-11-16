use tatara_lisp_eval::words::returned_to_lisp;
use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Arity, Interpreter, Value};

fn fresh() -> Interpreter<()> {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i
}

fn run(i: &mut Interpreter<()>, src: &str) -> String {
    let forms = read_spanned(src).expect("reads");
    format!("{}", i.eval_program(&forms, &mut ()).expect("runs"))
}

fn run_vm(i: &mut Interpreter<()>, src: &str) -> String {
    let forms = read_spanned(src).expect("reads");
    format!("{}", i.eval_program_vm(&forms, &mut ()).expect("runs"))
}

#[test]
fn with_nothing_registered_again_every_word_stays_native() {
    let i = fresh();
    assert!(
        returned_to_lisp(&i).is_empty(),
        "{:?}",
        returned_to_lisp(&i)
    );
}

#[test]
fn an_embedder_s_cdr_is_what_rest_and_its_dependents_call() {
    let mut i = fresh();
    i.register_fn("cdr", Arity::Exact(1), |a: &[Value], (): &mut (), _| {
        Ok(match &a[0] {
            Value::List(xs) => Value::list(xs.iter().skip(1).cloned()),
            _ => Value::list(Vec::<Value>::new()),
        })
    });
    let lisp = returned_to_lisp(&i);
    for word in ["rest", "second", "last", "butlast"] {
        assert!(lisp.iter().any(|w| w == word), "{word} not in {lisp:?}");
    }
    assert!(!lisp.iter().any(|w| w == "inc"), "{lisp:?}");
    assert_eq!(run(&mut i, "(rest (list 1))"), "()");
    assert_eq!(run_vm(&mut i, "(rest (list 1))"), "()");
    assert_eq!(run(&mut i, "(second (list 1 2 3))"), "2");
    assert_eq!(run(&mut i, "(inc 41)"), "42");
}

#[test]
fn an_embedder_s_equality_is_what_membership_and_position_use() {
    let mut i = fresh();
    assert_eq!(
        run(&mut i, "(member? 1.0 (list 1 2))"),
        run(&mut fresh(), "(member? 1.0 (list 1 2))")
    );
    i.register_fn("equal?", Arity::Exact(2), |a: &[Value], (): &mut (), _| {
        let num = |v: &Value| match v {
            Value::Int(n) => i32::try_from(*n).ok().map(f64::from),
            Value::Float(f) => Some(*f),
            _ => None,
        };
        Ok(Value::Bool(match (num(&a[0]), num(&a[1])) {
            (Some(x), Some(y)) => (x - y).abs() < f64::EPSILON,
            _ => format!("{}", a[0]) == format!("{}", a[1]),
        }))
    });
    assert_eq!(run(&mut i, "(member? 1.0 (list 1 2))"), "#t");
    assert_eq!(run(&mut i, "(position 2.0 (list 1 2))"), "1");
    assert_eq!(run_vm(&mut i, "(position 2.0 (list 1 2))"), "1");
}

#[test]
fn a_word_the_embedder_replaces_is_the_embedder_s_and_takes_its_dependents_back() {
    let mut i = fresh();
    i.register_fn("count", Arity::Exact(1), |_: &[Value], (): &mut (), _| {
        Ok(Value::Int(-1))
    });
    assert_eq!(run(&mut i, "(count (list 1 2 3))"), "-1");
    assert!(!returned_to_lisp(&i).iter().any(|w| w == "count"));
}

#[test]
fn a_fork_keeps_the_words_its_parent_returned() {
    let mut parent = fresh();
    parent.register_fn("cdr", Arity::Exact(1), |_: &[Value], (): &mut (), _| {
        Ok(Value::Int(7))
    });
    let mut child = parent.fork();
    assert_eq!(run(&mut child, "(rest (list 1 2))"), "7");
}

#[test]
fn an_alias_taken_before_the_override_follows_the_word_back() {
    let mut i = fresh();
    run(&mut i, "(define nx rest)");
    i.register_fn("cdr", Arity::Exact(1), |_: &[Value], (): &mut (), _| {
        Ok(Value::list(Vec::<Value>::new()))
    });
    assert_eq!(run(&mut i, "(nx (list 1))"), "()");
    assert_eq!(run(&mut i, "(next (list 1))"), "()");
    assert_eq!(run_vm(&mut i, "(next (list 1))"), "()");
}
