use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, Value};

struct NoHost;

fn run(src: &str, vm: bool) -> Result<Value, String> {
    let mut i: Interpreter<NoHost> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut NoHost);
    let forms = read_spanned(src).map_err(|e| format!("{e:?}"))?;
    let out = if vm {
        i.eval_program_vm(&forms, &mut NoHost)
    } else {
        i.eval_program(&forms, &mut NoHost)
    };
    out.map_err(|e| format!("{e}"))
}

fn agree(name: &str, src: &str, expected: &str) {
    let tree = run(src, false).map(|v| format!("{v}"));
    let vm = run(src, true).map(|v| format!("{v}"));
    assert_eq!(
        tree.as_deref(),
        Ok(expected),
        "case {name}: tree-walker gave {tree:?}"
    );
    assert_eq!(tree, vm, "case {name}: tree={tree:?} vm={vm:?}");
}

const CASES: &[(&str, &str, &str)] = &[
    (
        "callee-does-not-clobber-caller",
        "(define (g) (define k 2) k)
         (define (f) (define k 1) (g) k)
         (f)",
        "1",
    ),
    (
        "callee-does-not-clobber-caller-recursive",
        "(define (walk n)
           (define k n)
           (if (= n 0) 0 (begin (walk (- n 1)) k)))
         (walk 3)",
        "3",
    ),
    (
        "param-redefined-in-place",
        "(define (f k) (define k (+ k 1)) k)
         (f 1)",
        "2",
    ),
    (
        "redefinition-reads-the-local",
        "(define (f) (define x 1) (define x (+ x 10)) x)
         (f)",
        "11",
    ),
    (
        "right-side-reads-the-outer-binding",
        "(define k 10)
         (define (f) (define k (+ k 1)) k)
         (list (f) k)",
        "(11 10)",
    ),
    (
        "define-does-not-leak-to-global",
        "(define k :global)
         (define (f) (define k :local) k)
         (list (f) k)",
        "(:local :global)",
    ),
    (
        "define-in-let-stays-in-let",
        "(define k :global)
         (define (f) (let ((a 1)) (define k 2) k))
         (list (f) k)",
        "(2 :global)",
    ),
    (
        "define-in-top-level-let-stays-in-let",
        "(define k :global)
         (list (let ((a 1)) (define k 2) k) k)",
        "(2 :global)",
    ),
    (
        "local-function-recurses",
        "(define (go) :global)
         (define (f n)
           (define (go i acc) (if (= i 0) acc (go (- i 1) (+ acc i))))
           (go n 0))
         (list (f 4) (go))",
        "(10 :global)",
    ),
    (
        "local-lambda-value-recurses",
        "(define (f n)
           (define loop (lambda (i acc) (if (= i 0) acc (loop (- i 1) (* acc 2)))))
           (loop n 1))
         (f 5)",
        "32",
    ),
    (
        "local-functions-mutually-recurse",
        "(define (parity n)
           (define (ev? i) (if (= i 0) :even (od? (- i 1))))
           (define (od? i) (if (= i 0) :odd (ev? (- i 1))))
           (ev? n))
         (list (parity 4) (parity 7))",
        "(:even :odd)",
    ),
    (
        "closure-sees-later-redefinition",
        "(define (f)
           (define x 1)
           (define g (lambda () x))
           (define x 2)
           (g))
         (f)",
        "2",
    ),
    (
        "nested-function-locals-are-separate",
        "(define (outer)
           (define k 1)
           (define (inner) (define k 2) k)
           (list (inner) k))
         (outer)",
        "(2 1)",
    ),
    (
        "inner-closure-reads-outer-local",
        "(define (outer)
           (define k 5)
           (define (inner) (+ k 1))
           (inner))
         (outer)",
        "6",
    ),
    (
        "top-level-define-is-global",
        "(define k 1)
         (define (f) k)
         (define k 2)
         (f)",
        "2",
    ),
];

#[test]
fn a_body_define_binds_in_its_own_frame_on_both_evaluators() {
    for (name, src, expected) in CASES {
        agree(name, src, expected);
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const NAMES: &[&str] = &["k", "x", "acc"];

fn expr(rng: &mut Rng, bound: &[&str], callable: usize, depth: usize) -> String {
    let leaf = depth == 0 || rng.below(3) == 0;
    if leaf {
        return if bound.is_empty() || rng.below(4) == 0 {
            format!("{}", rng.below(9))
        } else {
            bound[rng.below(bound.len())].to_string()
        };
    }
    match rng.below(4) {
        0 if callable > 0 => {
            let j = rng.below(callable);
            format!("(f{j} {})", expr(rng, bound, callable, depth - 1))
        }
        1 => format!(
            "(if (> {} 3) {} {})",
            expr(rng, bound, callable, depth - 1),
            expr(rng, bound, callable, depth - 1),
            expr(rng, bound, callable, depth - 1)
        ),
        _ => format!(
            "(+ {} {})",
            expr(rng, bound, callable, depth - 1),
            expr(rng, bound, callable, depth - 1)
        ),
    }
}

fn function(rng: &mut Rng, i: usize) -> String {
    let mut bound: Vec<&str> = vec!["p"];
    let mut body = Vec::new();
    for _ in 0..=rng.below(4) {
        let name = NAMES[rng.below(NAMES.len())];
        match rng.below(4) {
            0 => {
                let inner = NAMES[rng.below(NAMES.len())];
                let e = expr(rng, &["q", inner], i, 2);
                body.push(format!("(define (h{i} q) (define {inner} (+ q 1)) {e})"));
                let call = expr(rng, &bound, i, 1);
                body.push(format!("(define {name} (h{i} {call}))"));
            }
            1 => {
                let e = expr(rng, &bound, i, 2);
                body.push(format!("(let ((t {e})) (define {name} (+ t 1)) {name})"));
            }
            _ => {
                let e = expr(rng, &bound, i, 3);
                body.push(format!("(define {name} {e})"));
                if !bound.contains(&name) {
                    bound.push(name);
                }
            }
        }
    }
    let weighted: Vec<String> = bound
        .iter()
        .enumerate()
        .map(|(w, n)| format!("(* {} {n})", w + 1))
        .collect();
    body.push(format!("(+ {})", weighted.join(" ")));
    format!("(define (f{i} p) {})", body.join(" "))
}

fn program(seed: u64) -> String {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let count = 2 + rng.below(4);
    let mut out = String::from("(define k :gk) (define x :gx) (define acc :gacc) ");
    for i in 0..count {
        out.push_str(&function(&mut rng, i));
        out.push(' ');
    }
    let last = count - 1;
    format!("{out}(list (f{last} {}) k x acc)", rng.below(9))
}

#[test]
fn vm_and_walker_agree_on_nested_function_locals_with_shared_names() {
    for seed in 1..=400u64 {
        let src = program(seed);
        let tree = run(&src, false).map(|v| format!("{v}"));
        let vm = run(&src, true).map(|v| format!("{v}"));
        assert!(
            tree.is_ok(),
            "seed {seed}: tree-walker failed {tree:?}\n{src}"
        );
        assert_eq!(tree, vm, "seed {seed}: tree={tree:?} vm={vm:?}\n{src}");
    }
}
