//! Updating a value in place when it is held once changes nothing a program
//! can observe.
//!
//! A tail call to a primitive that takes its arguments by value (`append`,
//! `cons`, `hash-map-set`, `hash-map-remove`, `hash-map-merge`) runs after the
//! calling frame is released (`TailResult::Apply`). So `(define (put m k v) (hash-map-set m k v))`
//! hands `hash-map-set` a map nothing else holds whenever its caller passed a
//! temporary, and the update lands in that map's own allocation instead of a
//! copy. That is only correct if every version a program can still name is
//! left exactly as it was, which is what this file holds against a model.
//!
//! The corpus is seeded and random: each step takes a version some global still
//! names, runs it through a chain of one to four helper calls (every
//! intermediate in the chain is a temporary, so every update after the first
//! is in place), binds the result to a new global, and sometimes aliases a
//! version under a second name first. At the end every version is read back
//! and compared with a model built from plain copied `Vec`s and `BTreeMap`s.
//!
//! ## Red run, recorded (2026-10-10)
//!
//! With `map_cow`'s uniqueness test widened to "held at most twice" (an
//! `unsafe` write through `Arc::as_ptr` when `Arc::strong_count(&arc) <= 2`), a
//! version some global still named was mutated under it:
//!
//! ```text
//! test every_named_version_survives_in_place_updates ... FAILED
//! seed 11400714819323198485: map version m2 changed
//!   left: {48: 40}
//!  right: {}
//! ```

use std::collections::BTreeMap;

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, MapKey, Value};

const HELPERS: &str = "
(define (push-back xs x) (append xs (list x)))
(define (push-front xs x) (cons x xs))
(define (join2 xs ys) (append xs ys))
(define (put m k v) (assoc m k v))
(define (del m k) (hash-map-remove m k))
(define (put-pair m k v) (put (put m k v) (+ k 1) v))
(define (fold-back xs ys) (foldl push-back xs ys))
";

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

    fn small(&mut self) -> i64 {
        (self.next() % 50) as i64
    }
}

fn run(i: &mut Interpreter<()>, src: &str) -> Value {
    let forms = read_spanned(src).expect("reads");
    i.eval_program(&forms, &mut ())
        .unwrap_or_else(|e| panic!("{src}\n  failed: {e}"))
}

fn ints(v: &Value) -> Vec<i64> {
    match v {
        Value::Nil => Vec::new(),
        Value::List(xs) => xs
            .iter()
            .map(|x| match x {
                Value::Int(n) => *n,
                other => panic!("not an int: {other:?}"),
            })
            .collect(),
        other => panic!("not a list: {other:?}"),
    }
}

fn int_map(v: &Value) -> BTreeMap<i64, i64> {
    match v {
        Value::Map(m) => m
            .iter()
            .map(|(k, v)| match (k, v) {
                (MapKey::Int(k), Value::Int(v)) => (*k, *v),
                other => panic!("not an int entry: {other:?}"),
            })
            .collect(),
        other => panic!("not a map: {other:?}"),
    }
}

/// One link of a list chain: the source text wrapping `inner`, and the same
/// operation on the model.
fn list_link(rng: &mut Rng, inner: String, model: &mut Vec<i64>) -> String {
    match rng.below(4) {
        0 => {
            let x = rng.small();
            model.push(x);
            format!("(push-back {inner} {x})")
        }
        1 => {
            let x = rng.small();
            model.insert(0, x);
            format!("(push-front {inner} {x})")
        }
        2 => {
            let ys: Vec<i64> = (0..rng.below(4)).map(|_| rng.small()).collect();
            model.extend(&ys);
            let ys: Vec<String> = ys.iter().map(i64::to_string).collect();
            format!("(fold-back {inner} (list {}))", ys.join(" "))
        }
        _ => {
            let ys: Vec<i64> = (0..1 + rng.below(70)).map(|_| rng.small()).collect();
            model.extend(&ys);
            let ys: Vec<String> = ys.iter().map(i64::to_string).collect();
            format!("(join2 {inner} (list {}))", ys.join(" "))
        }
    }
}

fn map_link(rng: &mut Rng, inner: String, model: &mut BTreeMap<i64, i64>) -> String {
    let k = rng.small();
    let v = rng.small();
    match rng.below(3) {
        0 => {
            model.insert(k, v);
            format!("(put {inner} {k} {v})")
        }
        1 => {
            model.remove(&k);
            format!("(del {inner} {k})")
        }
        _ => {
            model.insert(k, v);
            model.insert(k + 1, v);
            format!("(put-pair {inner} {k} {v})")
        }
    }
}

fn differential(seed: u64, steps: usize) {
    let mut rng = Rng(seed);
    let mut i = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    run(&mut i, HELPERS);

    let mut lists: Vec<Vec<i64>> = vec![Vec::new()];
    let mut maps: Vec<BTreeMap<i64, i64>> = vec![BTreeMap::new()];
    run(&mut i, "(define l0 (list)) (define m0 (hash-map))");

    for _ in 0..steps {
        if rng.below(2) == 0 {
            let mut from = rng.below(lists.len());
            if rng.below(5) == 0 {
                run(&mut i, &format!("(define l{} l{from})", lists.len()));
                lists.push(lists[from].clone());
                from = lists.len() - 1;
            }
            let mut model = lists[from].clone();
            let mut src = format!("l{from}");
            for _ in 0..1 + rng.below(4) {
                src = list_link(&mut rng, src, &mut model);
            }
            run(&mut i, &format!("(define l{} {src})", lists.len()));
            lists.push(model);
        } else {
            let mut from = rng.below(maps.len());
            if rng.below(5) == 0 {
                run(&mut i, &format!("(define m{} m{from})", maps.len()));
                maps.push(maps[from].clone());
                from = maps.len() - 1;
            }
            let mut model = maps[from].clone();
            let mut src = format!("m{from}");
            for _ in 0..1 + rng.below(4) {
                src = map_link(&mut rng, src, &mut model);
            }
            run(&mut i, &format!("(define m{} {src})", maps.len()));
            maps.push(model);
        }
    }

    for (n, want) in lists.iter().enumerate() {
        let got = ints(&run(&mut i, &format!("l{n}")));
        assert_eq!(&got, want, "seed {seed}: list version l{n} changed");
    }
    for (n, want) in maps.iter().enumerate() {
        let got = int_map(&run(&mut i, &format!("m{n}")));
        assert_eq!(&got, want, "seed {seed}: map version m{n} changed");
    }
}

#[test]
fn every_named_version_survives_in_place_updates() {
    for seed in [0x9E37_79B9_7F4A_7C15, 0xD1B5_4A32_D192_ED03, 7, 42, 2026] {
        differential(seed, 400);
    }
}
