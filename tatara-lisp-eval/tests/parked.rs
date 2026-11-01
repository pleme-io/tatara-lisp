use std::collections::VecDeque;
use std::sync::Arc;

use tatara_lisp_eval::ffi::Arity;
use tatara_lisp_eval::vm::{
    compile_program, Budget, Chunk, FnRef, ParkError, Parked, ParkedValue, Progress, Vm, VmError,
};
use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, Value};

#[derive(Default)]
struct Mail(VecDeque<i64>);

fn mail(items: &[i64]) -> Mail {
    Mail(items.iter().copied().collect())
}

fn node(with_mail: bool) -> Interpreter<Mail> {
    let mut interp = Interpreter::new();
    install_full_stdlib_with(&mut interp, &mut Mail::default());
    if with_mail {
        interp.register_awaitable_fn(
            "next-mail",
            Arity::Exact(0),
            |_args: &[Value], mail: &Mail| !mail.0.is_empty(),
            |_args: &[Value], mail: &mut Mail, _span| {
                Ok(Value::Int(mail.0.pop_front().expect("ready said yes")))
            },
        );
    }
    interp.register_fn(
        "handle",
        Arity::Exact(0),
        |_args: &[Value], _mail: &mut Mail, _span| Ok(Value::Foreign(Arc::new(7_u8))),
    );
    interp
}

fn compile(interp: &mut Interpreter<Mail>, source: &str) -> Arc<Chunk> {
    let forms = read_spanned(source).expect("parse");
    let expanded = interp
        .expand_program(&forms, &mut Mail::default())
        .expect("expand");
    Arc::new(compile_program(&expanded).expect("compile"))
}

fn finish(
    vm: &mut Vm,
    chunk: &Arc<Chunk>,
    interp: &mut Interpreter<Mail>,
    mail: &mut Mail,
) -> String {
    loop {
        match vm.resume(Arc::clone(chunk), interp, mail).expect("resume") {
            Progress::Done(value) => return value.to_string(),
            Progress::Yielded => {}
            Progress::Blocked => panic!("blocked with {} messages waiting", mail.0.len()),
        }
    }
}

fn uninterrupted(source: &str, messages: &[i64]) -> String {
    let mut interp = node(true);
    let chunk = compile(&mut interp, source);
    let mut vm = Vm::new();
    finish(&mut vm, &chunk, &mut interp, &mut mail(messages))
}

fn park_at_next_mail(source: &str) -> Parked {
    let mut interp = node(true);
    let chunk = compile(&mut interp, source);
    let mut vm = Vm::new();
    let progress = vm
        .step(Arc::clone(&chunk), &mut interp, &mut Mail::default())
        .expect("step");
    assert!(matches!(progress, Progress::Blocked), "got {progress:?}");
    vm.parked(&chunk).expect("parked")
}

fn over_the_wire(parked: &Parked) -> Parked {
    let wire = serde_json::to_string(parked).expect("serialize");
    let back: Parked = serde_json::from_str(&wire).expect("deserialize");
    assert_eq!(&back, parked, "the wire form loses nothing");
    back
}

fn resume_elsewhere(source: &str, parked: &Parked, messages: &[i64]) -> String {
    let mut there = node(true);
    let chunk = compile(&mut there, source);
    let mut vm = Vm::from_parked(parked, &chunk, &there, Budget::default()).expect("from_parked");
    finish(&mut vm, &chunk, &mut there, &mut mail(messages))
}

fn closures(value: &ParkedValue, out: &mut Vec<(usize, Vec<usize>)>) {
    match value {
        ParkedValue::Closure { func, captures } => out.push((*func, captures.clone())),
        ParkedValue::List(items) => items.iter().for_each(|item| closures(item, out)),
        _ => {}
    }
}

const NESTED: &str = "
(let ((scale (lambda (x) (* x 10)))
      (wait (lambda (k) (let ((m (next-mail))) (+ m k)))))
  ((lambda (base)
     (try
       (let ((got (wait base)))
         (list base (scale got) (wait 0)))
       (catch (e) :caught)))
   7))";

#[test]
fn a_process_parked_at_a_host_call_finishes_on_a_fresh_vm_as_an_uninterrupted_run_does() {
    let expected = uninterrupted(NESTED, &[5, 6]);
    assert_eq!(expected, "(7 120 6)");

    let parked = over_the_wire(&park_at_next_mail(NESTED));
    assert_eq!(parked.frames.len(), 3, "top level, the lambda, and wait");
    assert_eq!(parked.frames[0].func, FnRef::Top);
    assert!(
        parked.frames.iter().any(|frame| !frame.handlers.is_empty()),
        "the try handler is active at the park"
    );

    assert_eq!(resume_elsewhere(NESTED, &parked, &[5, 6]), expected);
}

#[test]
fn a_resumed_vm_runs_under_the_budget_its_caller_passes() {
    let parked = park_at_next_mail(NESTED);
    let mut there = node(true);
    let chunk = compile(&mut there, NESTED);
    let budget = Budget {
        fuel: Some(3),
        ..Budget::default()
    };
    let mut vm = Vm::from_parked(&parked, &chunk, &there, budget).expect("from_parked");
    assert_eq!(vm.burned(), 0, "nothing of the new budget is spent yet");
    let err = vm
        .resume(chunk, &mut there, &mut mail(&[5, 6]))
        .expect_err("three instructions cannot finish it");
    assert!(
        matches!(err, VmError::BudgetExhausted { limit: 3, .. }),
        "got {err:?}"
    );
}

const COUNTER: &str = "
(let ((make-counter (lambda ()
                      (let ((n 0))
                        (list (lambda () (set! n (+ n 1)) n)
                              (lambda () n)))))
      (bump-after-mail (lambda (inc) (let ((m (next-mail))) (inc) m))))
  ((lambda ()
     (let ((c (make-counter)))
       (let ((inc (car c)) (peek (car (cdr c))))
         (inc)
         (let ((m (bump-after-mail inc)))
           (list m (peek) (inc))))))))";

#[test]
fn closures_that_shared_a_captured_cell_share_it_after_the_move() {
    let expected = uninterrupted(COUNTER, &[77]);
    assert_eq!(expected, "(77 2 3)");

    let parked = over_the_wire(&park_at_next_mail(COUNTER));
    assert!(parked.frames.len() >= 3, "nested frames at the park");
    let mut found = Vec::new();
    parked
        .stack
        .iter()
        .chain(&parked.cells)
        .for_each(|value| closures(value, &mut found));
    let shared = found.iter().any(|a| {
        found
            .iter()
            .any(|b| a.0 != b.0 && a.1.len() == 1 && a.1 == b.1)
    });
    assert!(
        shared,
        "inc and peek are recorded as capturing one cell: {found:?}"
    );

    assert_eq!(resume_elsewhere(COUNTER, &parked, &[77]), expected);
}

#[test]
fn a_native_function_the_receiving_interpreter_lacks_is_refused_by_name() {
    let parked = park_at_next_mail("(+ 1 (next-mail))");
    assert!(parked
        .stack
        .contains(&ParkedValue::Native("next-mail".to_string())));

    let mut there = node(false);
    let chunk = compile(&mut there, "(+ 1 (next-mail))");
    let err = Vm::from_parked(&parked, &chunk, &there, Budget::default())
        .err()
        .expect("refused");
    assert_eq!(
        err,
        ParkError::UnknownNative {
            name: "next-mail".to_string()
        }
    );
    assert!(err.to_string().contains("`next-mail`"), "{err}");
}

#[test]
fn a_value_with_no_data_form_is_refused_by_name() {
    let cases = [
        ("(let ((h (handle))) (next-mail) h)", "a host handle"),
        ("(let ((f compose)) (next-mail) f)", "a tree-walker closure"),
        ("(let ((p (delay 1))) (next-mail) p)", "a promise"),
    ];
    for (source, kind) in cases {
        let mut interp = node(true);
        let chunk = compile(&mut interp, source);
        let mut vm = Vm::new();
        let progress = vm
            .step(Arc::clone(&chunk), &mut interp, &mut Mail::default())
            .expect("step");
        assert!(
            matches!(progress, Progress::Blocked),
            "{source}: {progress:?}"
        );
        match vm.parked(&chunk) {
            Err(ParkError::NoDataForm { what, at }) => {
                assert_eq!(what, kind, "{source}");
                assert!(at.starts_with("stack slot"), "{source}: {at}");
            }
            other => panic!("{source}: expected a refusal naming {kind}, got {other:?}"),
        }
    }
}

const PROMOTED: &str = "
(let ((h (handle)))
  (let ((read-h (lambda () h)))
    (set! h 5)
    (+ (next-mail) (read-h))))";

#[test]
fn a_captured_local_is_carried_by_its_cell_not_by_the_slot_it_left() {
    let expected = uninterrupted(PROMOTED, &[1]);
    assert_eq!(expected, "6");
    let parked = over_the_wire(&park_at_next_mail(PROMOTED));
    assert_eq!(resume_elsewhere(PROMOTED, &parked, &[1]), expected);
}

#[test]
fn a_parked_state_from_other_source_is_refused_by_name() {
    let parked = park_at_next_mail("(+ (next-mail) 1)");
    let mut there = node(true);
    let other = compile(&mut there, "(+ (next-mail) 2)");
    let err = Vm::from_parked(&parked, &other, &there, Budget::default())
        .err()
        .expect("refused");
    assert_eq!(
        err,
        ParkError::ChunkMismatch {
            parked: parked.chunk,
            chunk: other.fingerprint(),
        }
    );

    let mut forged = parked.clone();
    forged.chunk = other.fingerprint();
    forged.frames[0].func = FnRef::Table(9);
    let err = Vm::from_parked(&forged, &other, &there, Budget::default())
        .err()
        .expect("refused");
    assert!(matches!(err, ParkError::Malformed(_)), "got {err:?}");
}
