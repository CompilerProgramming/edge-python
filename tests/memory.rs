use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use compiler::lexer::lex;
use compiler::parser::Parser;
use compiler::vm::VM;
use compiler::vm::types::Limits;

/* Counts the bytes alive, so a test reads what a program really holds. */
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        LIVE.fetch_add(size, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/* What `build` holds once it ran, by the model and for real, both past an empty program of the same shape. */
fn held(build: &str) -> (usize, usize) {
    let run = |src: &str| {
        let (tokens, _) = lex(src);
        let (chunk, errs) = Parser::new(src, tokens.into_iter()).parse();
        assert!(errs.is_empty(), "{src}");
        let before = LIVE.load(Ordering::Relaxed);
        let mut vm = VM::with_limits(&chunk, Limits::sandbox());
        vm.run().unwrap();
        (vm.memory(), LIVE.load(Ordering::Relaxed) - before)
    };
    let (model, real) = run(&build.replace("N", "100000"));
    let (model0, real0) = run(&build.replace("N", "0"));
    (model - model0, real.saturating_sub(real0))
}

#[test]
fn the_memory_model_never_counts_less_than_what_a_program_holds() {
    let shapes = [
        ("tuples of 4 floats", "xs = [(float(i), 1.0, 2.0, 3.0) for i in range(N)]"),
        ("dicts of 4 keys", "xs = [{'o': float(i), 'h': 1.0, 'l': 2.0, 'c': 3.0} for i in range(N)]"),
        ("columns of floats", "o = [float(i) for i in range(N)]\nh = [float(i) for i in range(N)]"),
        ("strings of 20 chars", "xs = [str(i).rjust(20, 'x') for i in range(N)]"),
        ("instances of 3 attributes", "class B:\n    def __init__(self, i):\n        self.a = i\n        self.b = 1.0\n        self.c = 'x'\nxs = [B(i) for i in range(N)]"),
        ("lists of one item", "xs = [[i] for i in range(N)]"),
        ("a dict of int keys", "d = {i: i for i in range(N)}"),
        ("a set of ints", "s = {i for i in range(N)}"),
    ];
    let mut short = Vec::new();
    for (name, src) in shapes {
        let (model, real) = held(src);
        println!("{name:28} model {model:>10} real {real:>10} ratio {:.2}", model as f64 / real.max(1) as f64);
        // A list counts exactly its capacity, so a byte of rounding is all the slack it gets.
        if model.saturating_mul(100) < real.saturating_mul(99) { short.push(format!("{name}: model {model} < real {real}")); }
    }
    assert!(short.is_empty(), "{}", short.join("\n"));
}

#[test]
fn the_peak_keeps_what_a_program_held_after_a_collection_let_it_go() {
    // The list goes before the loop starts collecting, so only the peak still remembers it.
    let src = "xs = [0] * 1000000\nxs = None\nn = 0\nfor i in range(100000):\n    n += len([i])\n";
    let (tokens, _) = lex(src);
    let (chunk, errs) = Parser::new(src, tokens.into_iter()).parse();
    assert!(errs.is_empty());
    let mut vm = VM::with_limits(&chunk, Limits::sandbox());
    vm.run().unwrap();
    assert!(vm.memory_peak() >= 8_000_000, "peak {}", vm.memory_peak());
    assert!(vm.memory() < 4_000_000, "held {}", vm.memory());
}
