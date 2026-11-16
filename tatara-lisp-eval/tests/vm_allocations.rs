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

fn allocations_running(src: &str) -> usize {
    let mut i: Interpreter<()> = Interpreter::new();
    install_full_stdlib_with(&mut i, &mut ());
    let forms = read_spanned(src).expect("reads");
    let before = ALLOCATIONS.with(Cell::get);
    i.eval_program_vm(&forms, &mut ()).expect("runs");
    ALLOCATIONS.with(Cell::get) - before
}

fn per_step(steps: usize, program: impl Fn(usize) -> String) -> usize {
    let small = allocations_running(&program(10));
    let large = allocations_running(&program(10 + steps));
    large.saturating_sub(small) / steps
}

#[test]
fn a_counted_loop_on_the_vm_allocates_nothing_per_step() {
    let steps = per_step(20_000, |n| {
        format!(
            "(define (down n acc) (if (= n 0) acc (down (- n 1) (+ acc (* 2 n))))) (down {n} 0)"
        )
    });
    assert_eq!(steps, 0);
}

#[test]
fn a_non_tail_recursive_call_allocates_nothing_per_call() {
    let steps = per_step(2_000, |n| {
        format!("(define (depth n) (if (< n 1) 0 (+ 1 (depth (- n 1))))) (depth {n})")
    });
    assert_eq!(steps, 0);
}

#[test]
fn an_operator_on_the_stack_floor_runs_in_place() {
    let in_place = allocations_running("(+ 1.5 2.5)");
    let called = allocations_running("(+ 1 2.5)");
    assert!(in_place < called, "in place {in_place}, called {called}");
}
