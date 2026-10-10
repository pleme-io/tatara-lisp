//! A program's own accumulator reaches an owned primitive held once, so the
//! update lands in place.
//!
//! `tests/owned_args.rs` proves the primitive half: handed an unaliased map
//! by the host, `hash-map-set` updates it in place. It says nothing about a
//! Lisp program, and a Lisp program never handed one over. In
//! `(foldl (lambda (acc i) (assoc acc 7 i)) m xs)` the accumulator had a
//! second holder at every step: the closure bound its parameters by cloning
//! them out of an argument vector that stayed alive for the whole body, and
//! the frame binding `acc` was still alive while `hash-map-set` ran — and
//! `assoc` is itself a closure, so it happened twice per step.
//!
//! Now a tail call to an owned primitive runs after the calling frame is
//! released (`TailResult::Apply`), so neither holder is left when it runs.
//! This file measures what that buys on the same loop with and without a
//! second holder: the SHARED arm keeps every previous version alive through a
//! global, so it must copy, and it is also exactly what every step cost
//! before.
//!
//! Single `#[test]`: a process-wide allocation counter cannot be read
//! correctly from tests running in parallel.
//!
//! ## Green, and the red run, recorded (2026-10-10, debug build)
//!
//! ```text
//! bytes per step, n=4096: append unique 893 / shared 4185; assoc unique 1339 / shared 7888
//! ```
//!
//! With `eval_in_tail` calling an owned primitive in place instead of
//! returning `TailResult::Apply` (the frame still alive while it runs), the
//! two arms converge exactly, and the gate fails on the list half:
//!
//! ```text
//! bytes per step, n=4096: append unique 4185 / shared 4185; assoc unique 7888 / shared 7888
//! an unaliased accumulator must not copy its list's tail on every append
//!   (unique 4185 B/step, shared 4185 B/step)
//! ```
//!
//! Moving arguments into the frame instead of cloning them is NOT what this
//! measures: with the clone put back the gate stays green (989 / 4281,
//! 1579 / 8128), because the argument vector is gone before `Apply` runs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter, Value};

static BYTES: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ARMED.load(Relaxed) == 1 {
            BYTES.fetch_add(l.size(), Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if ARMED.load(Relaxed) == 1 {
            BYTES.fetch_add(new.saturating_sub(l.size()), Relaxed);
        }
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const BASE: usize = 4096;
const STEPS: usize = 2000;

fn run(i: &mut Interpreter<()>, src: &str) -> Value {
    let forms = read_spanned(src).expect("reads");
    i.eval_program(&forms, &mut ())
        .unwrap_or_else(|e| panic!("{src}\n  failed: {e}"))
}

/// Bytes allocated per step by `(foldl step base (range 0 STEPS))`, after
/// `setup` has built `base` and defined `step`.
fn bytes_per_step(setup: &str) -> usize {
    let mut i = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    run(&mut i, setup);
    run(&mut i, "(define xs (range 0 2000))");
    let forms = read_spanned("(define out (foldl step base xs))").expect("reads");
    BYTES.store(0, Relaxed);
    ARMED.store(1, Relaxed);
    i.eval_program(&forms, &mut ()).expect("the fold runs");
    ARMED.store(0, Relaxed);
    BYTES.load(Relaxed) / STEPS
}

const LIST_BASE: &str =
    "(define base (foldl (lambda (acc i) (append acc (list i))) (list) (range 0 4096)))";
const MAP_BASE: &str =
    "(define base (foldl (lambda (acc i) (assoc acc i i)) (hash-map) (range 0 4096)))";

#[test]
fn an_accumulator_held_once_is_updated_in_place() {
    // ── lists: `append` one element per step ──
    let list_unique = bytes_per_step(&format!(
        "{LIST_BASE} (define (step acc i) (append acc (list i)))"
    ));
    let list_shared = bytes_per_step(&format!(
        "{LIST_BASE} (define keep 0) \
         (define (step acc i) (begin (set! keep acc) (append acc (list i))))"
    ));

    // ── maps: `assoc` (a closure over `hash-map-set`) on an existing key ──
    let map_unique = bytes_per_step(&format!("{MAP_BASE} (define (step acc i) (assoc acc 7 i))"));
    let map_shared = bytes_per_step(&format!(
        "{MAP_BASE} (define keep 0) \
         (define (step acc i) (begin (set! keep acc) (assoc acc 7 i)))"
    ));

    eprintln!(
        "bytes per step, n={BASE}: append unique {list_unique} / shared {list_shared}; \
         assoc unique {map_unique} / shared {map_shared}"
    );

    assert!(
        list_unique * 2 < list_shared,
        "an unaliased accumulator must not copy its list's tail on every append \
         (unique {list_unique} B/step, shared {list_shared} B/step)"
    );
    assert!(
        map_unique * 2 < map_shared,
        "an unaliased accumulator must not copy its map's path on every assoc \
         (unique {map_unique} B/step, shared {map_shared} B/step)"
    );

    // The values are right either way, and the shared arm's kept version is
    // the one before the last step, intact.
    let mut i = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    run(&mut i, &format!("{MAP_BASE} (define keep 0)"));
    run(
        &mut i,
        "(define (step acc i) (begin (set! keep acc) (assoc acc 7 i))) \
         (define out (foldl step base (range 0 10)))",
    );
    let got = run(
        &mut i,
        "(list (get out 7) (get keep 7) (get base 7) (count out))",
    );
    assert_eq!(format!("{got}"), "(9 8 7 4096)");
}
