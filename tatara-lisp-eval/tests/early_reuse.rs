//! Early reuse (blue docs/PURE-BLUE.md M3a): a parameter read for the last
//! time outside tail position moves out of its frame, so a copy-on-write
//! update on it lands in place.
//!
//! `tests/tail_reuse.rs` covers tail position, where the frame is released
//! before the primitive runs. Here the update is an argument of another call,
//! `(car (list (assoc acc 7 i)))`, so the frame binding `acc` is alive while
//! `assoc` runs, and without early reuse every step copies the map's path.
//!
//! Single `#[test]` for the allocation half: a process-wide counter cannot be
//! read correctly from tests running in parallel.

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

const STEPS: usize = 2000;

const SETUP: &str =
    "(define base (foldl (lambda (acc i) (assoc acc i i)) (hash-map) (range 0 4096))) \
     (define (step acc i) (car (list (assoc acc 7 i)))) \
     (define xs (range 0 2000))";

fn interpreter(early: bool) -> Interpreter<()> {
    let mut i = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    i.set_early_reuse(early);
    i
}

fn run(i: &mut Interpreter<()>, src: &str) -> Value {
    let forms = read_spanned(src).expect("reads");
    i.eval_program(&forms, &mut ())
        .unwrap_or_else(|e| panic!("{src}\n  failed: {e}"))
}

fn bytes_per_step(early: bool) -> usize {
    let mut i = interpreter(early);
    run(&mut i, SETUP);
    let forms = read_spanned("(define out (foldl step base xs))").expect("reads");
    BYTES.store(0, Relaxed);
    ARMED.store(1, Relaxed);
    i.eval_program(&forms, &mut ()).expect("the fold runs");
    ARMED.store(0, Relaxed);
    BYTES.load(Relaxed) / STEPS
}

#[test]
fn a_non_tail_update_of_a_parameter_read_last_lands_in_place() {
    let off = bytes_per_step(false);
    let on = bytes_per_step(true);
    eprintln!("bytes per step, non-tail assoc on a 4096-key map: off {off} / on {on}");
    assert!(
        on * 2 < off,
        "with early reuse the accumulator is held once when assoc runs, so it must not \
         copy the map's path every step (on {on} B/step, off {off} B/step)"
    );

    for early in [false, true] {
        let mut i = interpreter(early);
        run(&mut i, SETUP);
        let got = run(
            &mut i,
            "(define out (foldl step base (range 0 10))) (list (get out 7) (get base 7) (count out))",
        );
        assert_eq!(format!("{got}"), "(9 7 4096)", "early reuse {early}");
    }

    for early in [false, true] {
        let mut i = interpreter(early);
        let got = run(
            &mut i,
            "(define (both m) (list (hash-map-set m :a 1) m)) \
             (define kept (hash-map :b 2)) \
             (define r (both kept)) \
             (list (get (car r) :a) (get (car (cdr r)) :a) (get kept :a) (get (car (cdr r)) :b))",
        );
        assert_eq!(
            format!("{got}"),
            "(1 () () 2)",
            "an earlier read is never moved, and the caller's value is intact (early reuse {early})"
        );
    }
}
