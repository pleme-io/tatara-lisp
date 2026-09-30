//! `Value::List` and `Value::Map` are persistent: updates share structure, and
//! value semantics are exactly what they were.
//!
//! Measured before (tatara-script 0.3.64, release), building by `foldl`:
//! `append` 1.04 / 2.49 / 10.13 s and `hash-map-set` 1.00 / 3.78 / 15.05 s at
//! 10k / 20k / 40k elements — quadratic, because every update of a shared
//! `Arc<Vec>` / `Arc<HashMap>` copied it whole.
//!
//! Red run, recorded 2026-09-29: with `prim_append` restored to rebuilding a
//! `Vec` of every operand's elements, `building_is_not_quadratic` fails on the
//! list ratio (`append scales as 16.2x for 4x the elements`, 5k 3.1 s / 20k
//! 49.9 s in debug); the differential stays green, which is why the two are
//! separate gates.

use std::collections::BTreeMap;
use std::time::Instant;

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, MapKey, Value};

fn interp() -> Interpreter<()> {
    let mut i = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i
}

fn run(i: &mut Interpreter<()>, src: &str) -> Value {
    let forms = read_spanned(src).expect("reads");
    i.eval_program(&forms, &mut ()).expect("evaluates")
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

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Differential against the old representation's semantics — a `Vec` copied
/// on every update — on a seeded corpus. Each step derives a new version from
/// a RANDOM earlier one, so persistence is exercised: an old version must read
/// back unchanged after later versions were built from it.
#[test]
fn lists_behave_like_copied_vectors() {
    let mut rng = Rng(0x5eed_0000_0000_0007);
    let mut i = interp();
    let mut model: Vec<Vec<i64>> = vec![Vec::new()];
    run(&mut i, "(define v0 (list))");
    for n in 1..600 {
        let from = rng.below(model.len());
        let base = model[from].clone();
        let x = (rng.next() % 1000) as i64;
        let k = rng.below(base.len() + 2);
        let (expr, next): (String, Vec<i64>) = match rng.below(7) {
            0 => (
                format!("(append v{from} (list {x}))"),
                [base.clone(), vec![x]].concat(),
            ),
            1 => (
                format!("(cons {x} v{from})"),
                [vec![x], base.clone()].concat(),
            ),
            2 if !base.is_empty() => (format!("(cdr v{from})"), base[1..].to_vec()),
            3 => (
                format!("(take {k} v{from})"),
                base[..k.min(base.len())].to_vec(),
            ),
            4 => (
                format!("(drop {k} v{from})"),
                base[k.min(base.len())..].to_vec(),
            ),
            5 => (
                format!("(reverse v{from})"),
                base.iter().rev().copied().collect(),
            ),
            _ => {
                let other = rng.below(model.len());
                (
                    format!("(append v{from} v{other})"),
                    [base.clone(), model[other].clone()].concat(),
                )
            }
        };
        run(&mut i, &format!("(define v{n} {expr})"));
        model.push(next);
        // Reads: nth and length agree too.
        let v = i.lookup_global(&format!("v{n}")).unwrap();
        assert_eq!(ints(&v), model[n], "v{n} = {expr}");
        if let Some(idx) = (!model[n].is_empty()).then(|| rng.below(model[n].len())) {
            let got = run(&mut i, &format!("(nth {idx} v{n})"));
            assert!(matches!(got, Value::Int(g) if g == model[n][idx]));
        }
    }
    // Every earlier version is intact.
    for (n, want) in model.iter().enumerate() {
        let v = i.lookup_global(&format!("v{n}")).unwrap();
        assert_eq!(&ints(&v), want, "v{n} changed after later versions");
    }
}

#[test]
fn maps_behave_like_copied_hash_maps() {
    let mut rng = Rng(0x5eed_0000_0000_000b);
    let mut i = interp();
    let mut model: Vec<BTreeMap<i64, i64>> = vec![BTreeMap::new()];
    run(&mut i, "(define m0 (hash-map))");
    for n in 1..600 {
        let from = rng.below(model.len());
        let mut next = model[from].clone();
        let k = (rng.next() % 64) as i64;
        let v = (rng.next() % 1000) as i64;
        let expr = match rng.below(4) {
            0 | 1 => {
                next.insert(k, v);
                format!("(hash-map-set m{from} {k} {v})")
            }
            2 => {
                next.remove(&k);
                format!("(hash-map-remove m{from} {k})")
            }
            _ => {
                let other = rng.below(model.len());
                next.extend(model[other].clone());
                format!("(hash-map-merge m{from} m{other})")
            }
        };
        run(&mut i, &format!("(define m{n} {expr})"));
        model.push(next);
        let got = i.lookup_global(&format!("m{n}")).unwrap();
        assert_eq!(int_map(&got), model[n], "m{n} = {expr}");
    }
    for (n, want) in model.iter().enumerate() {
        let got = i.lookup_global(&format!("m{n}")).unwrap();
        assert_eq!(&int_map(&got), want, "m{n} changed after later versions");
    }
}

fn build_seconds(src_for: impl Fn(usize) -> String, n: usize) -> f64 {
    let mut i = interp();
    let forms = read_spanned(&src_for(n)).unwrap();
    let t = Instant::now();
    let v = i.eval_program(&forms, &mut ()).unwrap();
    let secs = t.elapsed().as_secs_f64();
    assert!(matches!(v, Value::Int(len) if len == n as i64), "{v:?}");
    secs
}

/// The timing law: a build 4x larger costs well under 16x (quadratic) — the
/// bound is 8x, room for noise above the ~4x of linear. Printed for the record.
#[test]
fn building_is_not_quadratic() {
    let list = |n: usize| {
        format!("(length (foldl (lambda (acc x) (append acc (list x))) (list) (range 0 {n})))")
    };
    let map = |n: usize| {
        format!("(hash-map-count (foldl (lambda (acc x) (hash-map-set acc x x)) (hash-map) (range 0 {n})))")
    };
    for (what, src) in [
        ("append", &list as &dyn Fn(usize) -> String),
        ("hash-map-set", &map),
    ] {
        // Warm once so first-touch costs are not in the small measurement.
        build_seconds(src, 1_000);
        let small = build_seconds(src, 5_000);
        let large = build_seconds(src, 20_000);
        let ratio = large / small;
        println!("{what}: 5k {small:.3}s, 20k {large:.3}s, ratio {ratio:.1}");
        assert!(
            ratio < 8.0,
            "{what} scales as {ratio:.1}x for 4x the elements"
        );
    }
}
