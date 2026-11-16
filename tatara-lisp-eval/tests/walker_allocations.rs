use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use tatara_lisp_eval::{install_full_stdlib_with, read_spanned, Interpreter};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations_walking(src: &str) -> usize {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    let forms = read_spanned(src).expect("reads");
    let before = ALLOCATIONS.with(Cell::get);
    i.eval_program(&forms, &mut ()).expect("runs");
    ALLOCATIONS.with(Cell::get) - before
}

fn per_step(steps: usize, program: impl Fn(usize) -> String) -> usize {
    let small = allocations_walking(&program(10));
    let large = allocations_walking(&program(10 + steps));
    large.saturating_sub(small) / steps
}

fn two_parameter_loop(n: usize) -> String {
    format!("(define (down n acc) (if (= n 0) acc (down (- n 1) (+ acc n)))) (down {n} 0)")
}

#[test]
fn a_one_parameter_call_on_the_walker_allocates_three_times() {
    let steps = per_step(2_000, |n| {
        format!("(define (depth n) (if (< n 1) 0 (+ 1 (depth (- n 1))))) (depth {n})")
    });
    assert_eq!(steps, 3);
}

#[test]
fn a_two_parameter_tail_call_on_the_walker_allocates_four_times() {
    assert_eq!(per_step(20_000, two_parameter_loop), 4);
}

#[test]
fn a_native_call_on_the_walker_allocates_nothing_for_its_arguments() {
    let with_natives = per_step(20_000, |n| {
        format!(
            "(define (down n acc) (if (= n 0) acc (down (- n 1) (+ acc (* 2 n) (- n 1) (modulo n 7))))) (down {n} 0)"
        )
    });
    assert_eq!(with_natives, per_step(20_000, two_parameter_loop));
}
