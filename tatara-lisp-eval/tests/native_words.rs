use std::sync::mpsc;
use std::time::{Duration, Instant};

use tatara_lisp_eval::value::PromiseState;
use tatara_lisp_eval::words::WORD_NAMES;
use tatara_lisp_eval::{
    install_full_stdlib_with, read_spanned, EvalError, Interpreter, MapKey, Span, Value,
};

const TWINS: &str = r#"
(define (old-identity x) x)
(define (old-tap f x)
  (f x)
  x)
(define (old-first xs) (car xs))
(define (old-rest xs) (cdr xs))
(define (old-second xs) (car (cdr xs)))
(define (old-third xs) (car (cdr (cdr xs))))
(define (old-fourth xs) (car (cdr (cdr (cdr xs)))))
(define (old-last xs) (if (null? (cdr xs)) (car xs) (old-last (cdr xs))))
(define (old-butlast xs)
  (if (null? (cdr xs))
    ()
    (cons (car xs) (old-butlast (cdr xs)))))
(define (old-safe-nth n xs)
  (cond
    ((null? xs) ())
    ((<= n 0) (car xs))
    (else (old-safe-nth (- n 1) (cdr xs)))))
(define (old-empty? xs) (null? xs))
(define (old-not-empty? xs) (not (old-empty? xs)))
(define (old-range &rest args)
  (cond
    ((= (length args) 1) (old-range-impl 0 (car args) 1))
    ((= (length args) 2) (old-range-impl (car args) (old-second args) 1))
    ((= (length args) 3) (old-range-impl (car args) (old-second args) (old-third args)))
    (else (error-msg "range: arity 1, 2, or 3"))))
(define (old-range-impl start end step)
  (cond
    ((and (> step 0) (>= start end)) ())
    ((and (< step 0) (<= start end)) ())
    (else (cons start (old-range-impl (+ start step) end step)))))
(define (old-repeat-list x n) (if (<= n 0) () (cons x (old-repeat-list x (- n 1)))))
(define (old-concat &rest lists) (apply append lists))
(define (old-member? x xs) (any? (lambda (e) (equal? e x)) xs))
(define (old-position x xs) (find-index (lambda (e) (equal? e x)) xs))
(define (old-zip xs ys) (map (lambda (a b) (list a b)) xs ys))
(define (old-interleave xs ys) (apply append (old-zip xs ys)))
(define (old-intersperse sep xs)
  (cond
    ((null? xs) ())
    ((null? (cdr xs)) (list (car xs)))
    (else (cons (car xs) (cons sep (old-intersperse sep (cdr xs)))))))
(define (old-flatten xs)
  (cond
    ((null? xs) ())
    ((list? (car xs)) (append (old-flatten (car xs)) (old-flatten (cdr xs))))
    (else (cons (car xs) (old-flatten (cdr xs))))))
(define (old-distinct xs)
  (foldl
    (lambda (acc x) (if (old-member? x acc) acc (append acc (list x))))
    (list)
    xs))
(define (old-max-by f xs)
  (foldl
    (lambda (best x) (if (> (f x) (f best)) x best))
    (car xs)
    (cdr xs)))
(define (old-min-by f xs)
  (foldl
    (lambda (best x) (if (< (f x) (f best)) x best))
    (car xs)
    (cdr xs)))
(define (old-inc x) (+ x 1))
(define (old-dec x) (- x 1))
(define (old-zero? x) (= x 0))
(define (old-positive? x) (> x 0))
(define (old-negative? x) (< x 0))
(define (old-even? x) (= 0 (modulo x 2)))
(define (old-odd? x) (= 1 (modulo x 2)))
(define (old-not= a b) (not (equal? a b)))
(define (old-some? x) (not (null? x)))
(define (old-plist-get plist key)
  (cond
    ((null? plist) ())
    ((null? (cdr plist)) ())
    ((equal? (car plist) key) (old-second plist))
    (else (old-plist-get (cdr (cdr plist)) key))))
(define (old-lazy-empty) ())
(define (old-lazy-empty? s) (null? s))
(define (old-lazy-head s) (car s))
(define (old-lazy-rest s) (force (old-second s)))
(define (old-lazy-take n s)
  (cond
    ((<= n 0) ())
    ((old-lazy-empty? s) ())
    (else (cons (old-lazy-head s) (old-lazy-take (- n 1) (old-lazy-rest s))))))
(define (old-lazy-drop n s)
  (cond
    ((<= n 0) s)
    ((old-lazy-empty? s) s)
    (else (old-lazy-drop (- n 1) (old-lazy-rest s)))))
(define (old-realize s)
  (if (old-lazy-empty? s)
    ()
    (cons (old-lazy-head s) (old-realize (old-lazy-rest s)))))
(define (old-assoc m k v) (hash-map-set m k v))
(define (old-dissoc m k) (hash-map-remove m k))
(define (old-get m k) (hash-map-get m k))
(define (old-get-in m path)
  (cond
    ((null? path) m)
    ((null? m) ())
    (else (old-get-in (hash-map-get m (car path)) (cdr path)))))
(define (old-assoc-in m path v)
  (cond
    ((null? path) v)
    ((null? (cdr path)) (hash-map-set (or m (hash-map)) (car path) v))
    (else
      (let ((sub (or (hash-map-get m (car path)) (hash-map))))
        (hash-map-set
          (or m (hash-map))
          (car path)
          (old-assoc-in
            sub
            (cdr path)
            v))))))
(define (old-update-in m path f) (old-assoc-in m path (f (old-get-in m path))))
(define (old-frequencies xs)
  (foldl
    (lambda (acc x)
      (hash-map-update
        acc
        x
        (lambda (n) (if (null? n) 1 (+ n 1)))))
    (hash-map)
    xs))
(define (old-group-by-into-map f xs)
  (foldl
    (lambda (acc x)
      (let* ((k (f x)) (current (or (hash-map-get acc k) (list))))
        (hash-map-set
          acc
          k
          (append current (list x)))))
    (hash-map)
    xs))
(define (old-into-hash-map xs)
  (cond
    ((null? xs) (hash-map))
    ((and (list? (car xs)) (= (length (car xs)) 2))
      (foldl
        (lambda (acc pair) (hash-map-set acc (car pair) (old-second pair)))
        (hash-map)
        xs))
    (else (apply hash-map xs))))
(define (old-select-keys m keys)
  (foldl
    (lambda (acc k)
      (if (hash-map-has? m k)
        (hash-map-set
          acc
          k
          (hash-map-get m k))
        acc))
    (hash-map)
    keys))
(define (old-zipmap keys vals)
  (foldl
    (lambda (acc pair) (hash-map-set acc (car pair) (old-second pair)))
    (hash-map)
    (old-zip keys vals)))
(define (old-count xs)
  (cond
    ((null? xs) 0)
    ((list? xs) (length xs))
    ((hash-map? xs) (hash-map-count xs))
    ((string? xs) (string-length xs))
    (else (error :count "not countable"))))
(define (old-unwrap-or v default) (if (null? v) default v))
"#;

const SETUP: &str = r"
(define trace ())
(define (traced x) (set! trace (cons x trace)) x)
(define not-a-number (- (* 1e308 10.0) (* 1e308 10.0)))
(define beyond-max (* 1e308 10.0))
";

const LISTS: &[&str] = &[
    "()",
    "(list)",
    "(list 1 2 3)",
    "(list 5)",
    "(list 1 1.0 1 \"a\" \"a\" :k :k)",
    "(list (list 1 2) (list 1 2) (list 3))",
    "(list () (list) 1 (list 2 (list 3 ())))",
    "(list :a 1 :b 2 :c)",
    "(list :a 1 :b (hash-map :c 2))",
    "(list (list :a 1) (list :b 2))",
    "(list (list :a) (list :b 2 3))",
    "(list (list 1 2) 5)",
    "(list (hash-map :a 1) (hash-map :a 1))",
    "(list 0.0 -0.0 not-a-number not-a-number)",
    "(list 3 -1 4 1 -5 9 2 6)",
    "(list 2.5 -1 4 1.5 9)",
    "(list #f () :x 7 \"s\")",
    "(list (list 1 (list 2)) (list 1 (list 2)))",
];

const NUMBERS: &[&str] = &[
    "0",
    "1",
    "-1",
    "2",
    "7",
    "-3",
    "4",
    "9223372036854775807",
    "-9223372036854775808",
    "0.0",
    "-0.0",
    "1.5",
    "-2.5",
    "2.0",
    "not-a-number",
    "beyond-max",
    "(- beyond-max)",
    "3",
];

const SMALL: &[&str] = &[
    "-6", "-2", "-1", "0", "1", "2", "3", "5", "8", "12", "-2.5", "0.5", "1.5", "3.0", "-1.5",
    "\"a\"", "()", ":a",
];

const MAPS: &[&str] = &[
    "(hash-map)",
    "(hash-map :a 1 :b (hash-map :c 2))",
    "(hash-map :a (hash-map :b (hash-map :c 3)))",
    "(hash-map 1 :one 2.5 :f \"s\" :str)",
    "(hash-map :a 1)",
    "()",
];

const KEYS: &[&str] = &[
    ":a", ":b", ":c", "1", "2.5", "\"s\"", "()", "#t", "(list 1)", ":z",
];

const PATHS: &[&str] = &[
    "(list :a)",
    "(list :a :b)",
    "(list :a :b :c)",
    "()",
    "(list :z)",
    "(list :z :y)",
    "(list 1)",
    "(list (list 1))",
    "(list :a (list 1))",
    "5",
];

const FUNCTIONS: &[&str] = &[
    "(lambda (x) x)",
    "(lambda (x) (* x 2))",
    "(lambda (x) (if (number? x) (- x) 0))",
    "traced",
    "(lambda (x) :k)",
    "(lambda (x) (list x))",
    "(lambda (x) (if (number? x) (even? x) x))",
    "car",
    "inc",
    "5",
];

const STREAMS: &[&str] = &[
    "(lazy-cons 1 (lazy-cons 2 (lazy-cons 3 (lazy-empty))))",
    "(lazy-cons :a (lazy-empty))",
    "(lazy-empty)",
    "(lazy-cons (traced 1) (lazy-cons (traced 2) ()))",
    "(list 1 2)",
    "(list 1)",
];

const OTHERS: &[&str] = &[
    "\"\"",
    "\"a\"",
    "\"h\u{e9}llo\"",
    ":a",
    ":count",
    "'x",
    "#t",
    "#f",
    "car",
    "(lambda (x) x)",
    "(ex-info \"boom\" ())",
    "(delay 1)",
];

#[derive(Clone, Copy)]
enum Cat {
    List,
    Number,
    Small,
    Map,
    Key,
    Path,
    Function,
    Stream,
    Any,
}

#[derive(Clone)]
enum Arg {
    Held(Value),
    Fresh(&'static str),
}

struct Lab {
    interp: Interpreter<()>,
    pools: Vec<(Cat, Vec<Value>)>,
    any: Vec<Value>,
    state: u64,
}

impl Lab {
    fn new() -> Self {
        let mut interp: Interpreter<()> = Interpreter::new();
        install_full_stdlib_with(&mut interp, &mut ());
        interp
            .eval_program(&read_spanned(TWINS).expect("twins read"), &mut ())
            .expect("twins evaluate");
        interp
            .eval_program(&read_spanned(SETUP).expect("setup reads"), &mut ())
            .expect("setup evaluates");
        let mut lab = Self {
            interp,
            pools: Vec::new(),
            any: Vec::new(),
            state: 0x5eed_0000_0000_0b0d,
        };
        for (cat, exprs) in [
            (Cat::List, LISTS),
            (Cat::Number, NUMBERS),
            (Cat::Small, SMALL),
            (Cat::Map, MAPS),
            (Cat::Key, KEYS),
            (Cat::Path, PATHS),
            (Cat::Function, FUNCTIONS),
            (Cat::Stream, STREAMS),
        ] {
            let values: Vec<Value> = exprs.iter().map(|e| lab.eval(e)).collect();
            if !matches!(cat, Cat::Small) {
                lab.any.extend(values.iter().cloned());
            }
            lab.pools.push((cat, values));
        }
        let others: Vec<Value> = OTHERS.iter().map(|e| lab.eval(e)).collect();
        lab.any.extend(others);
        lab
    }

    fn eval(&mut self, src: &str) -> Value {
        self.interp
            .eval_program(&read_spanned(src).expect("reads"), &mut ())
            .unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    fn next(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn draw(&mut self, cat: Cat) -> Arg {
        let r = self.next();
        if matches!(cat, Cat::Stream) {
            return Arg::Fresh(STREAMS[(r >> 8) as usize % STREAMS.len()]);
        }
        let strict = matches!(cat, Cat::Small);
        if matches!(cat, Cat::Any) || (!strict && r.is_multiple_of(5)) {
            return Arg::Held(self.any[(r >> 8) as usize % self.any.len()].clone());
        }
        let pool = &self
            .pools
            .iter()
            .find(|(c, _)| std::mem::discriminant(c) == std::mem::discriminant(&cat))
            .expect("pool")
            .1;
        Arg::Held(pool[(r >> 8) as usize % pool.len()].clone())
    }

    fn call(&mut self, name: &str, args: &[Arg]) -> String {
        self.eval("(set! trace ())");
        let args: Vec<Value> = args
            .iter()
            .map(|a| match a {
                Arg::Held(v) => v.clone(),
                Arg::Fresh(src) => self.eval(src),
            })
            .collect();
        self.eval("(set! trace ())");
        let f = self
            .interp
            .lookup_global(name)
            .unwrap_or_else(|| panic!("{name} is unbound"));
        let out = self
            .interp
            .apply_external_value(&f, args, &mut (), Span::synthetic());
        let trace = self.interp.lookup_global("trace").unwrap_or(Value::Nil);
        format!("{} | trace {}", outcome(&out), render(&trace))
    }
}

fn outcome(r: &Result<Value, EvalError>) -> String {
    match r {
        Ok(v) => format!("ok {}", render(v)),
        Err(EvalError::ArityMismatch { expected, got, .. }) => {
            format!("err arity {expected:?} got {got}")
        }
        Err(e) => format!("err {}", e.short_message()),
    }
}

fn render(v: &Value) -> String {
    match v {
        Value::Nil => "nil".into(),
        Value::Bool(b) => format!("{b}"),
        Value::Int(n) => format!("i{n}"),
        Value::Float(f) => format!("f{f:?}"),
        Value::Str(s) => format!("{s:?}"),
        Value::Symbol(s) => format!("'{s}"),
        Value::Keyword(s) => format!(":{s}"),
        Value::List(xs) => {
            let items: Vec<String> = xs.iter().map(render).collect();
            format!("[{}]", items.join(" "))
        }
        Value::Map(m) => {
            let mut items: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{} {}", render(&MapKey::to_value(k)), render(v)))
                .collect();
            items.sort();
            format!("{{{}}}", items.join(", "))
        }
        Value::Closure(c) => format!("closure@{:p}", std::sync::Arc::as_ptr(c)),
        Value::NativeFn(f) => format!("native {}", f.name),
        Value::Promise(p) => match &*p.lock().expect("promise lock") {
            PromiseState::Forced(v) => format!("promise={}", render(v)),
            PromiseState::Pending(_) => "promise pending".into(),
        },
        Value::Error(e) => {
            let data: Vec<String> = e
                .data
                .iter()
                .map(|(k, v)| format!("{} {}", render(k), render(v)))
                .collect();
            format!("error({} {:?} [{}])", e.tag, e.message, data.join(" "))
        }
        Value::Sexp(s, _) => format!("sexp {s}"),
        Value::Foreign(_) => "foreign".into(),
    }
}

fn spec(name: &str) -> &'static [Cat] {
    use Cat::{Any, Function, Key, List, Map, Number, Path, Small, Stream};
    match name {
        "identity" | "empty?" | "not-empty?" | "some?" | "count" => &[Any],
        "tap" => &[Function, Any],
        "first" | "rest" | "second" | "third" | "fourth" | "last" | "butlast" | "flatten"
        | "distinct" | "frequencies" | "into-hash-map" => &[List],
        "safe-nth" => &[Number, List],
        "range" | "range-impl" => &[Small, Small, Small],
        "repeat-list" => &[Any, Small],
        "concat" => &[List, List, List],
        "member?" | "position" | "intersperse" => &[Any, List],
        "zip" | "interleave" | "zipmap" => &[List, List],
        "max-by" | "min-by" | "group-by-into-map" => &[Function, List],
        "inc" | "dec" | "zero?" | "positive?" | "negative?" | "even?" | "odd?" => &[Number],
        "not=" | "unwrap-or" => &[Any, Any],
        "plist-get" => &[List, Key],
        "lazy-empty" => &[],
        "lazy-empty?" | "lazy-head" | "lazy-rest" | "realize" => &[Stream],
        "lazy-take" | "lazy-drop" => &[Small, Stream],
        "assoc" => &[Map, Key, Any],
        "dissoc" | "get" => &[Map, Key],
        "get-in" | "select-keys" => &[Map, Path],
        "assoc-in" => &[Map, Path, Any],
        "update-in" => &[Map, Path, Function],
        other => panic!("no corpus spec for {other}"),
    }
}

fn variadic(name: &str) -> bool {
    matches!(name, "range" | "concat")
}

fn checked_words() -> Vec<&'static str> {
    let mut words: Vec<&'static str> = WORD_NAMES.to_vec();
    words.push("not=");
    words
}

#[test]
fn every_word_is_a_native_after_the_full_install() {
    let mut sorted = WORD_NAMES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, WORD_NAMES, "WORD_NAMES is sorted and unique");
    let mut interp: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut interp, &mut ());
    for name in checked_words() {
        let v = interp
            .lookup_global(name)
            .unwrap_or_else(|| panic!("{name} is unbound"));
        assert!(
            matches!(v, Value::NativeFn(_)),
            "{name} is a Lisp closure again: {v:?}"
        );
    }
}

#[test]
fn every_word_agrees_with_its_lisp_twin_on_a_seeded_corpus() {
    let mut lab = Lab::new();
    let mut mismatches = Vec::new();
    let mut tally = Vec::new();
    let mut refused = 0usize;
    for name in checked_words() {
        let cats = spec(name);
        let (mut ok, mut err) = (0usize, 0usize);
        for round in 0..400 {
            let arity_drawn = if variadic(name) {
                round % (cats.len() + 2)
            } else {
                cats.len()
            };
            let args: Vec<Arg> = (0..arity_drawn)
                .map(|i| lab.draw(cats[i.min(cats.len().saturating_sub(1))]))
                .collect();
            let lisp = lab.call(&format!("old-{name}"), &args);
            let native = lab.call(name, &args);
            if lisp.starts_with("ok") {
                ok += 1;
            } else {
                err += 1;
            }
            if refused_where_the_lisp_ran_out_of_depth(&lisp, &native) {
                refused += 1;
            } else if lisp != native {
                let shown: Vec<String> = args
                    .iter()
                    .map(|a| match a {
                        Arg::Held(v) => render(v),
                        Arg::Fresh(src) => (*src).to_string(),
                    })
                    .collect();
                mismatches.push(format!(
                    "({name} {})\n  lisp:   {lisp}\n  native: {native}",
                    shown.join(" ")
                ));
            }
        }
        if !variadic(name) {
            for arity_drawn in 0..=4 {
                if arity_drawn == cats.len() {
                    continue;
                }
                let args = vec![Arg::Held(Value::Int(1)); arity_drawn];
                let lisp = lab.call(&format!("old-{name}"), &args);
                let native = lab.call(name, &args);
                if lisp != native {
                    mismatches.push(format!(
                        "({name} with {arity_drawn} args)\n  lisp:   {lisp}\n  native: {native}"
                    ));
                }
            }
        }
        tally.push((name, ok, err));
    }
    assert!(
        mismatches.is_empty(),
        "{} disagreements:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    for (name, ok, _) in &tally {
        assert!(*ok > 0, "{name}: the corpus never reached a success");
    }
    let errs: usize = tally.iter().map(|(_, _, e)| e).sum();
    let oks: usize = tally.iter().map(|(_, o, _)| o).sum();
    eprintln!("{oks} agreeing successes, {errs} agreeing errors, {refused} zero steps refused");
    assert!(refused > 0, "the corpus never drew a step of zero");
}

fn refused_where_the_lisp_ran_out_of_depth(lisp: &str, native: &str) -> bool {
    lisp.contains("call depth budget") && native.contains("the step never reaches the end")
}

#[test]
fn infinite_streams_agree_with_the_lisp_readers() {
    let mut lab = Lab::new();
    for src in [
        "(list (lazy-take 5 (iterate-lazy inc 0)) (old-lazy-take 5 (iterate-lazy inc 0)))",
        "(list (lazy-head (lazy-drop 7 (iterate-lazy inc 0))) (old-lazy-head (old-lazy-drop 7 (iterate-lazy inc 0))))",
        "(list (lazy-take 7 (cycle (list 1 2 3))) (old-lazy-take 7 (cycle (list 1 2 3))))",
        "(list (lazy-take 3 (repeat-lazy :x)) (old-lazy-take 3 (repeat-lazy :x)))",
        "(list (realize (lazy-cons 1 (lazy-cons 2 ()))) (old-realize (lazy-cons 1 (lazy-cons 2 ()))))",
        "(list (lazy-take 4 (lazy-map inc (iterate-lazy inc 0))) (old-lazy-take 4 (lazy-map inc (iterate-lazy inc 0))))",
        "(list (lazy-take 3 (lazy-filter even? (iterate-lazy inc 1))) (old-lazy-take 3 (lazy-filter old-even? (iterate-lazy inc 1))))",
    ] {
        let v = lab.eval(src);
        let Value::List(pair) = &v else {
            panic!("{src}: {v}")
        };
        assert_eq!(render(&pair[0]), render(&pair[1]), "{src}");
    }
}

#[test]
fn a_step_that_never_reaches_the_end_is_refused() {
    let mut lab = Lab::new();
    for src in [
        "(range 0 5 0)",
        "(range 0.0 5 0.0)",
        "(range 0 5 not-a-number)",
        "(range not-a-number 5 1)",
        "(range 1e300 1e301 1.0)",
        "(range-impl 3 1 0)",
        "(repeat-list :x not-a-number)",
        "(repeat-list :x 1e300)",
    ] {
        let forms = read_spanned(src).expect("reads");
        let err = lab.interp.eval_program(&forms, &mut ()).expect_err(src);
        assert!(
            err.short_message()
                .contains("the step never reaches the end"),
            "{src}: {err}"
        );
    }
    assert_eq!(
        render(&lab.eval("(range 0 3 0.5)")),
        "[i0 f0.5 f1.0 f1.5 f2.0 f2.5]"
    );
    assert_eq!(render(&lab.eval("(repeat-list :x 2.5)")), "[:x :x :x]");
}

const SIZE: i64 = 200_000;

fn within_bound(name: &str, build: impl FnOnce() -> (String, Vec<String>) + Send + 'static) {
    let bound = Duration::from_secs(30);
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(move || {
            let (word, args) = build();
            let mut interp: Interpreter<()> = Interpreter::new();
            install_full_stdlib_with(&mut interp, &mut ());
            interp
                .eval_program(&read_spanned(SETUP).expect("reads"), &mut ())
                .expect("setup");
            let values: Vec<Value> = args
                .iter()
                .map(|src| {
                    interp
                        .eval_program(&read_spanned(src).expect("reads"), &mut ())
                        .unwrap_or_else(|e| panic!("{src}: {e}"))
                })
                .collect();
            let f = interp.lookup_global(&word).expect("bound");
            let start = Instant::now();
            let out = interp.apply_external_value(&f, values, &mut (), Span::synthetic());
            let took = start.elapsed();
            let _ = tx.send((outcome(&out).chars().take(80).collect::<String>(), took));
        })
        .expect("spawns");
    match rx.recv_timeout(bound) {
        Ok((shown, took)) => {
            assert!(shown.starts_with("ok"), "{name}: {shown}");
            eprintln!("{name}: {took:?}");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{name} panicked"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{name} did not finish {SIZE} elements within {bound:?}")
        }
    }
}

fn row(name: &str, word: &str, args: &[&str]) {
    let word = word.to_string();
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    within_bound(name, move || (word, args));
}

#[test]
fn list_words_stay_linear_at_two_hundred_thousand_elements() {
    let xs = format!("(range {SIZE})");
    let half = format!("(map (lambda (i) (modulo i {})) (range {SIZE}))", SIZE / 2);
    let pairs = format!("(map (lambda (i) (list i i)) (range {SIZE}))");
    let plist = format!("(apply append (map (lambda (i) (list i i)) (range {SIZE})))");
    let big_map = format!("(into-hash-map (map (lambda (i) (list i i)) (range {SIZE})))");
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        ("first", "first", vec![&xs]),
        ("rest", "rest", vec![&xs]),
        ("second", "second", vec![&xs]),
        ("third", "third", vec![&xs]),
        ("fourth", "fourth", vec![&xs]),
        ("last", "last", vec![&xs]),
        ("butlast", "butlast", vec![&xs]),
        ("safe-nth", "safe-nth", vec!["199999", &xs]),
        ("safe-nth float", "safe-nth", vec!["199998.5", &xs]),
        ("empty?", "empty?", vec![&xs]),
        ("not-empty?", "not-empty?", vec![&xs]),
        ("range", "range", vec!["200000"]),
        ("range float", "range", vec!["0.0", "200000", "1.0"]),
        ("range-impl", "range-impl", vec!["0", "200000", "1"]),
        ("repeat-list", "repeat-list", vec![":x", "200000"]),
        ("repeat-list float", "repeat-list", vec![":x", "200000.0"]),
        ("concat", "concat", vec![&xs, &xs, &xs]),
        ("member?", "member?", vec!["-1", &xs]),
        ("position", "position", vec!["-1", &xs]),
        ("zip", "zip", vec![&xs, &xs]),
        ("interleave", "interleave", vec![&xs, &xs]),
        ("intersperse", "intersperse", vec!["0", &xs]),
        ("flatten", "flatten", vec![&pairs]),
        ("distinct", "distinct", vec![&half]),
        ("distinct pairs", "distinct", vec![&pairs]),
        ("max-by", "max-by", vec!["identity", &xs]),
        ("min-by", "min-by", vec!["identity", &xs]),
        ("plist-get", "plist-get", vec![&plist, "-1"]),
        ("frequencies", "frequencies", vec![&half]),
        ("group-by-into-map", "group-by-into-map", vec!["even?", &xs]),
        ("into-hash-map", "into-hash-map", vec![&pairs]),
        ("select-keys", "select-keys", vec![&big_map, &xs]),
        ("zipmap", "zipmap", vec![&xs, &xs]),
        ("count", "count", vec![&xs]),
        (
            "lazy-take",
            "lazy-take",
            vec!["200000", "(iterate-lazy inc 0)"],
        ),
        (
            "lazy-drop",
            "lazy-drop",
            vec!["200000", "(iterate-lazy inc 0)"],
        ),
    ];
    for (name, word, args) in rows {
        row(name, word, &args);
    }
}

#[test]
fn vm_closures_pass_through_the_higher_order_words() {
    let fresh = || {
        let mut i: Interpreter<()> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut ());
        i
    };
    for src in [
        "(max-by (lambda (x) (- x)) (list 3 1 2))",
        "(min-by (lambda (x) (* x x)) (list -3 1 2))",
        "(update-in (hash-map :a (hash-map :b 1)) (list :a :b) (lambda (n) (+ n 41)))",
        "(hash-map-get (group-by-into-map (lambda (x) (even? x)) (range 10)) #t)",
        "(define total 0) (tap (lambda (x) (set! total (+ total x))) 5) total",
        "(lazy-take 4 (iterate-lazy (lambda (x) (* 2 x)) 1))",
        "(realize (lazy-cons 1 (lazy-cons 2 (lazy-empty))))",
        "(define (step i acc) (if (= i 0) acc (step (dec i) (+ acc (count (zip (list i) (list i))) (first (list i)) (get (assoc (hash-map) :k i) :k) (if (even? i) 1 0))))) (step 1000 0)",
        "(max-by first (list))",
    ] {
        let forms = read_spanned(src).expect("reads");
        let walker = outcome(&fresh().eval_program(&forms, &mut ()));
        let vm = outcome(&fresh().eval_program_vm(&forms, &mut ()));
        assert_eq!(walker, vm, "{src}");
        assert!(walker.starts_with("ok") || src.contains("(list))"), "{src}: {walker}");
    }
}
