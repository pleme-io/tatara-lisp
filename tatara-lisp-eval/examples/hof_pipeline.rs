use std::time::Instant;

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, Value};

const PIPELINE: &str = "(reduce (lambda (a x) (+ a x)) 0 (filter (lambda (x) (= 0 (modulo x 2))) (map (lambda (x) (* x 3)) xs)))";

fn run(vm: bool) -> (Value, f64) {
    let mut interp: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut interp, &mut ());
    interp.define_global(
        "xs",
        Value::list((0..1_000_000).map(Value::Int).collect::<Vec<_>>()),
    );
    let forms = read_spanned(PIPELINE).expect("read");
    let start = Instant::now();
    let value = if vm {
        interp.eval_program_vm(&forms, &mut ())
    } else {
        interp.eval_program(&forms, &mut ())
    }
    .expect("eval");
    (value, start.elapsed().as_secs_f64() * 1000.0)
}

fn main() {
    let rounds: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let only = std::env::args().nth(2);
    for (label, vm) in [("walker", false), ("vm", true)] {
        if only.as_deref().is_some_and(|o| o != label) {
            continue;
        }
        let mut best = f64::MAX;
        let mut answer = Value::Nil;
        for _ in 0..rounds {
            let (value, ms) = run(vm);
            best = best.min(ms);
            answer = value;
        }
        println!("{label}\t{best:.1} ms\t{answer:?}");
    }
}
