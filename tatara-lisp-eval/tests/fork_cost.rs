//! `fork` must stay cheap, because cheapness is the entire reason it exists.
//!
//! The correctness gates live in `fork.rs`; this one guards the *cost*. A
//! future change that makes `fork` deep-copy the globals would keep every
//! correctness test green — a deep copy is, if anything, more isolated — and
//! silently return the per-child cost this was built to remove.

use std::time::{Duration, Instant};

use tatara_lisp_eval::{install_full_stdlib_with, Interpreter};

fn build_from_scratch() -> Interpreter<()> {
    let mut interp = Interpreter::new();
    let mut host = ();
    install_full_stdlib_with(&mut interp, &mut host);
    interp
}

fn per_iteration(n: u32, mut f: impl FnMut()) -> Duration {
    // One untimed pass so neither arm pays first-touch page faults.
    f();
    let t = Instant::now();
    for _ in 0..n {
        f();
    }
    t.elapsed() / n
}

/// The measured ratio is ~54x on an M-series laptop in release mode. The bound
/// is set at 5x deliberately: a wall-clock assertion on shared CI hardware is
/// the classic flaky test, so this is sized to catch a *structural* regression
/// — fork gaining a deep copy, or re-running the stdlib — and nothing subtler.
/// Missing the real figure by 10x is the price of it never failing spuriously.
const MINIMUM_SPEEDUP: f64 = 5.0;

#[test]
fn forking_is_far_cheaper_than_building() {
    const N: u32 = 200;

    let build = per_iteration(N, || {
        std::hint::black_box(build_from_scratch());
    });

    let prototype = build_from_scratch();
    let fork = per_iteration(N, || {
        std::hint::black_box(prototype.fork());
    });

    let speedup = build.as_secs_f64() / fork.as_secs_f64().max(f64::MIN_POSITIVE);
    println!("full build {build:?} · fork {fork:?} · {speedup:.1}x");

    assert!(
        speedup >= MINIMUM_SPEEDUP,
        "fork must stay structurally cheaper than a full build: \
         build {build:?}, fork {fork:?}, only {speedup:.1}x \
         (floor {MINIMUM_SPEEDUP}x). Did fork start copying the globals?"
    );
}
