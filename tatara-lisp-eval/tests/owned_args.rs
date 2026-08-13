//! Does an owned primitive actually update an unaliased payload in place?
//!
//! ## Why this file exists at all, and why it counts bytes
//!
//! The first version of this gate compared `Arc::as_ptr` before and after the
//! call: same allocation ⇒ in place, different ⇒ copied. It was **vacuous**,
//! and the red run is what proved it. With `map_cow` forced down its copy arm,
//! `unique_argument_is_updated_in_place` still PASSED:
//!
//! ```text
//! test map::tests::unique_argument_is_updated_in_place ... ok      ← should have been red
//! test map::tests::remove_and_merge_take_the_same_two_branches ... FAILED
//!   assertion `left == right` failed: unaliased merge must be in place
//!     left: 0xc90842510   right: 0xc90842250
//! ```
//!
//! The copy arm clones the table, then drops the sole `Arc`, then allocates a
//! new one **of exactly the size just freed** — so the allocator hands back the
//! same address and pointer identity is forged. It held only for `merge`,
//! whose extra operand allocations perturb the free list enough to break the
//! coincidence. A gate that passes on one primitive and fails on its identical
//! sibling is measuring the allocator, not the code.
//!
//! Allocation VOLUME cannot be forged that way: copying an n-entry `HashMap`
//! allocates a table sized for n, in place allocates nothing. So this file
//! carries a counting global allocator and measures bytes.
//!
//! ## The shared arm IS the "before"
//!
//! Every call took the copy path before this change — a borrowed `&[Value]`
//! argument is itself a second reference, so `Arc::get_mut` could never
//! succeed. The shared-vs-unique numbers printed below are therefore a real
//! before/after, measured in one binary against one build.
//!
//! Single `#[test]` on purpose: a process-wide allocation counter cannot be
//! read correctly from tests running in parallel.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

use tatara_lisp_eval::ffi::Arity;
use tatara_lisp_eval::value::NativeFn;
use tatara_lisp_eval::{install_map, install_primitives, Interpreter, MapKey, Span, Value};

// ── Counting allocator ─────────────────────────────────────────────────

static BYTES: AtomicUsize = AtomicUsize::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ARMED.load(Relaxed) == 1 {
            BYTES.fetch_add(l.size(), Relaxed);
            COUNT.fetch_add(1, Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if ARMED.load(Relaxed) == 1 {
            BYTES.fetch_add(new.saturating_sub(l.size()), Relaxed);
            COUNT.fetch_add(1, Relaxed);
        }
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Bytes and allocation count charged while `f` runs.
fn measure<R>(f: impl FnOnce() -> R) -> (usize, usize, R) {
    BYTES.store(0, Relaxed);
    COUNT.store(0, Relaxed);
    ARMED.store(1, Relaxed);
    let r = f();
    ARMED.store(0, Relaxed);
    (BYTES.load(Relaxed), COUNT.load(Relaxed), r)
}

// ── Fixtures ───────────────────────────────────────────────────────────

struct NoHost;

fn interp() -> Interpreter<NoHost> {
    let mut i: Interpreter<NoHost> = Interpreter::new();
    install_primitives(&mut i);
    install_map(&mut i);
    i
}

fn sample_map(n: usize) -> Value {
    let mut m = HashMap::with_capacity(n);
    for k in 0..n {
        let k = i64::try_from(k).expect("fixture size fits i64");
        m.insert(MapKey::Int(k), Value::Int(k));
    }
    Value::Map(Arc::new(m))
}

fn native(name: &str, arity: Arity) -> Value {
    Value::NativeFn(Arc::new(NativeFn {
        name: Arc::from(name),
        arity,
    }))
}

fn len_of(v: &Value) -> usize {
    match v {
        Value::Map(m) => m.len(),
        other => panic!("{other:?}"),
    }
}

// ── The gate ───────────────────────────────────────────────────────────

/// Both halves of the in-place claim, on independent evidence.
///
/// Green, 2026-08-13:
/// `hash-map-set on n=4096: unique 168 bytes / 2 allocs, shared 598256 bytes /
/// 4 allocs` — 3561× fewer bytes, and the two allocations left are the `Arc`
/// for the returned `Value` and one `Vec` for the argument list.
///
/// RED RUN 2026-08-13, both directions, each reddening exactly one half:
///
///   * `map_cow`'s in-place arm disabled — `Arc::get_mut(&mut arc)` replaced
///     by `Option::<&mut HashMap<MapKey, Value>>::None`. The unique half
///     FAILS, and the numbers converge exactly:
///     `hash-map-set on n=4096: unique 598256 bytes / 4 allocs, shared 598256
///      bytes / 4 allocs`
///     `an unaliased 4096-entry set must not copy the table (got 598256 bytes
///      in 4 allocations)`.
///     `map::tests::a_shared_map_is_never_mutated` stays GREEN, which is why
///     the shared half alone would not have caught this.
///
///   * `map_cow`'s copy arm rewritten to mutate through the shared `Arc` —
///     `let forced = Arc::as_ptr(&arc).cast_mut();` then
///     `Some(unsafe { &mut *forced })`. The SHARED half fails first, at
///     `the shared map was mutated`, and the unique measurement passes on the
///     way to it. In the unit tests the same mutation reddens
///     `a_shared_map_is_never_mutated` (`left: 3, right: 2`),
///     `a_named_binding_keeps_its_map` (`"(1 1 1)"` vs `"(1 2 1)"`) and the
///     pre-existing `hash_map_set_returns_new_map` (`"(2 2)"` vs `"(1 2)"`).
///
/// Neither mutation reddens both, which is the whole reason both are here.
#[test]
fn unaliased_updates_in_place_and_shared_ones_do_not() {
    const N: usize = 4096;
    let mut i = interp();
    let set = native("hash-map-set", Arity::Exact(3));

    // Warm every lazily-initialised path (interner, registry lookup, …) so
    // the measured window contains only the call under test.
    let warm = sample_map(4);
    let _ = i
        .apply_external_value(
            &set,
            vec![warm, Value::keyword("w"), Value::Int(0)],
            &mut NoHost,
            Span::synthetic(),
        )
        .unwrap();

    // ── unique: the argument is the only reference to its payload ──
    let m = sample_map(N);
    assert!(m.is_unique(), "fixture must start unaliased");
    let (uniq_bytes, uniq_allocs, out) = measure(|| {
        i.apply_external_value(
            &set,
            vec![m, Value::keyword("new"), Value::Int(9)],
            &mut NoHost,
            Span::synthetic(),
        )
        .unwrap()
    });
    assert_eq!(len_of(&out), N + 1);
    drop(out);

    // ── shared: somebody else still holds it (the pre-change behaviour) ──
    let m = sample_map(N);
    let kept = m.clone();
    assert!(!m.is_unique(), "fixture must start aliased");
    let (shared_bytes, shared_allocs, out) = measure(|| {
        i.apply_external_value(
            &set,
            vec![m, Value::keyword("new"), Value::Int(9)],
            &mut NoHost,
            Span::synthetic(),
        )
        .unwrap()
    });
    assert_eq!(len_of(&out), N + 1);
    assert_eq!(len_of(&kept), N, "the shared map was mutated");

    println!(
        "hash-map-set on n={N}: unique {uniq_bytes} bytes / {uniq_allocs} allocs, \
         shared {shared_bytes} bytes / {shared_allocs} allocs"
    );

    // An n=4096 `HashMap<MapKey, Value>` table is tens of KB. In place costs
    // nothing like that; the bound is loose so the gate tracks the branch
    // taken rather than an allocator's exact sizing.
    assert!(
        uniq_bytes < 4096,
        "an unaliased {N}-entry set must not copy the table \
         (got {uniq_bytes} bytes in {uniq_allocs} allocations)"
    );
    assert!(
        shared_bytes > 32_768,
        "a shared {N}-entry set must copy the table (got {shared_bytes} bytes)"
    );
    assert!(
        shared_bytes > uniq_bytes * 8,
        "shared {shared_bytes} vs unique {uniq_bytes} — the two arms are not \
         distinguishable, so one of them is not being taken"
    );
}

/// The end-to-end reading: a threaded update through the real pipeline.
///
/// `(hash-map-set (hash-map-set … m …) :x 0)` — the innermost set reads `m`
/// from its binding and must copy; every set above it works on a temporary
/// nobody else holds. So the chain costs ONE table copy no matter how long it
/// is.
///
/// Measured by **K-scaling**, which needs no second build to compare against:
/// under per-step copying the cost is linear in K, under copy-once it is flat.
/// A long chain and a short one are run against the *same* pre-built map, so
/// the difference between them contains only the chain.
///
/// Both engines, because they reach the same primitive by different paths and
/// the VM's own argument handling was half the fix.
///
/// Green, 2026-08-13:
/// ```text
/// tree-walker: n=1024, 2 sets 150447 B, 34 sets 161615 B
///              → 11168 B for 32 extra sets (349 B each); one table copy is 149744 B
/// vm:          n=1024, 2 sets 173807 B, 34 sets 205839 B
///              → 32032 B for 32 extra sets (1001 B each); one table copy is 149744 B
/// ```
/// The `2 sets` figure still contains one full copy — the innermost set reads
/// `m` from its binding, which is correct and must not change. What went away
/// is the *per-step* copy: 349 B against 149744 B is 429×, 1001 B is 150×.
///
/// RED RUNS 2026-08-13:
///
///   * `map_cow`'s in-place arm disabled — tree-walker charges
///     `149925 B each` for 32 extra sets against a `149744 B` table copy.
///     One copy per step, to within the noise. That number IS the behaviour
///     this change removed.
///
///   * the VM's `args_kept` restored to an unconditional `Some(args.clone())`
///     — the tree-walker arm stays flat at `349 B each` and the **vm arm alone**
///     fails at `150721 B each`. So the VM half of the fix is load-bearing and
///     separately gated: without it every VM call holds a second reference to
///     every argument and no owned primitive can ever see an unaliased one.
#[test]
fn a_threaded_update_chain_cost_does_not_grow_with_its_length() {
    use tatara_lisp_eval::read_spanned;

    const N: usize = 1024;
    const SHORT: usize = 2;
    const LONG: usize = 34;

    let base: String = {
        let pairs: String = (0..N).map(|k| format!(" {k} {k}")).collect();
        format!("(define m (hash-map{pairs}))")
    };
    let chain = |k: usize| {
        let mut s = String::from("(hash-map-count ");
        for _ in 0..k {
            s.push_str("(hash-map-set ");
        }
        s.push_str("m :x 0)");
        s.push_str(&" :x 0)".repeat(k - 1));
        s.push(')');
        read_spanned(&s).unwrap()
    };

    let base = read_spanned(&base).unwrap();
    let short = chain(SHORT);
    let long = chain(LONG);

    // One table copy at this size, for scale.
    let one_copy = {
        let m = sample_map(N);
        let _kept = m.clone();
        let mut i = interp();
        let set = native("hash-map-set", Arity::Exact(3));
        let (b, _, _) = measure(|| {
            i.apply_external_value(
                &set,
                vec![m, Value::keyword("x"), Value::Int(0)],
                &mut NoHost,
                Span::synthetic(),
            )
            .unwrap()
        });
        b
    };

    for engine in ["tree-walker", "vm"] {
        let run = |i: &mut Interpreter<NoHost>, forms: &[tatara_lisp_eval::Spanned]| {
            if engine == "vm" {
                i.eval_program_vm(forms, &mut NoHost).unwrap()
            } else {
                i.eval_program(forms, &mut NoHost).unwrap()
            }
        };

        let mut i = interp();
        run(&mut i, &base);
        // Warm: first chain also pays the one grow from n to n+1.
        run(&mut i, &short);

        let (short_bytes, _, v) = measure(|| run(&mut i, &short));
        assert_eq!(format!("{v}"), format!("{}", N + 1));
        let (long_bytes, _, v) = measure(|| run(&mut i, &long));
        assert_eq!(format!("{v}"), format!("{}", N + 1));

        let extra = long_bytes.saturating_sub(short_bytes);
        let steps = LONG - SHORT;
        println!(
            "{engine}: n={N}, {SHORT} sets {short_bytes} B, {LONG} sets {long_bytes} B \
             → {extra} B for {steps} extra sets ({} B each); one table copy is {one_copy} B",
            extra / steps
        );

        // Per-step copying would charge `one_copy` for each of the 32 extra
        // sets. Flat costs a few hundred bytes of compile/AST per step. An
        // eighth of one copy sits between the two by orders of magnitude.
        assert!(
            extra / steps < one_copy / 8,
            "{engine} paid {} B per extra set against a {one_copy} B table copy — \
             that is per-step copying, not copy-once",
            extra / steps
        );
    }
}

/// Wall-clock, for scale. `--ignored` because timing is not a gate: it is
/// machine- and load-dependent, and the byte counts above are the property
/// that must hold.
///
/// Run: `cargo test -p tatara-lisp-eval --release --test owned_args -- \
///       --ignored --nocapture`
///
/// Two workloads, because they answer different questions and only reporting
/// the flattering one would be dishonest:
///
///   * **threaded** — `m = set(m, …)` in a loop, the shape a `->` pipeline or
///     a fold produces. Nothing else holds `m`, nothing is discarded. This is
///     where the change pays.
///   * **discarded** — build a map, set one key, drop the result. The insert
///     stops copying but **reclaiming the map is still O(n)** and that cost is
///     untouched, so the ceiling here is ~2x no matter how large n gets.
///
/// The `shared` column is the pre-change behaviour exactly: under a borrowed
/// `&[Value]` every call saw an aliased argument, so every call copied.
///
/// Measured 2026-08-13, aarch64-darwin, `--release`, three consecutive runs:
///
/// ```text
///   workload       n    shared (us)    unique (us)    speedup
///   threaded    1024         16.899          0.304      55.6x
///  discarded    1024         17.462          8.156       2.1x
///   threaded   16384        259.484          0.307     846.1x
///  discarded   16384        270.787        126.385       2.1x
/// ```
/// (run 2: 54.8x / 2.1x / 848.1x / 2.1x — run 3: 55.6x / 2.1x / 859.2x / 2.1x)
///
/// The threaded `unique` column is **0.30 µs at both sizes** — an in-place
/// insert does not depend on n, which is the whole claim, visible directly.
///
/// Read the two workloads together. The allocation gate above reports 3561x
/// fewer bytes at n=4096, and `threaded` is where that converts into time.
/// `discarded` is the honest ceiling for code that throws its maps away:
/// **this change removes an allocation, not a deallocation** — reclaiming an
/// n-entry map is still O(n), so a build-then-discard loop cannot beat ~2x no
/// matter how large n gets.
#[test]
#[ignore = "timing, not a gate — see the byte-count tests above"]
fn timing_scale_of_the_two_arms() {
    use std::time::Instant;

    let mut i = interp();
    let set = native("hash-map-set", Arity::Exact(3));

    macro_rules! call {
        ($i:expr, $m:expr) => {
            $i.apply_external_value(
                &set,
                vec![$m, Value::keyword("x"), Value::Int(0)],
                &mut NoHost,
                Span::synthetic(),
            )
            .unwrap()
        };
    }

    println!(
        "{:>10} {:>7} {:>14} {:>14} {:>10}",
        "workload", "n", "shared (us)", "unique (us)", "speedup"
    );

    for n in [1024usize, 16384] {
        let reps = if n > 4096 { 300 } else { 3_000 };

        // ── threaded: the result feeds the next call, nothing is discarded ──
        let mut m = sample_map(n);
        let t = Instant::now();
        for _ in 0..reps {
            let keep = m.clone(); // a second reference, as before the change
            m = call!(i, m);
            drop(keep);
        }
        let shared = t.elapsed().as_secs_f64() / reps as f64 * 1e6;
        drop(m);

        let mut m = sample_map(n);
        let t = Instant::now();
        for _ in 0..reps {
            m = call!(i, m);
        }
        let unique = t.elapsed().as_secs_f64() / reps as f64 * 1e6;
        drop(m);
        println!(
            "{:>10} {n:>7} {shared:>14.3} {unique:>14.3} {:>9.1}x",
            "threaded",
            shared / unique
        );

        // ── discarded: every result is thrown away, so the O(n) drop stays ──
        let mut fixtures: Vec<Value> = (0..reps).map(|_| sample_map(n)).collect();
        let keep: Vec<Value> = fixtures.iter().cloned().collect();
        let t = Instant::now();
        for m in fixtures.drain(..) {
            std::hint::black_box(call!(i, m));
        }
        let shared = t.elapsed().as_secs_f64() / reps as f64 * 1e6;
        drop(keep);

        let mut fixtures: Vec<Value> = (0..reps).map(|_| sample_map(n)).collect();
        let t = Instant::now();
        for m in fixtures.drain(..) {
            std::hint::black_box(call!(i, m));
        }
        let unique = t.elapsed().as_secs_f64() / reps as f64 * 1e6;
        println!(
            "{:>10} {n:>7} {shared:>14.3} {unique:>14.3} {:>9.1}x",
            "discarded",
            shared / unique
        );
    }
}
