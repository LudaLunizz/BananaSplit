//! Proves the audio callback path never touches the heap: a counting global
//! allocator wraps the system allocator and we assert zero allocations and
//! zero frees across thousands of process() cycles with parameters changing
//! underneath (as the UI thread does).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use bananasplit::engine::{Engine, NUM_BUSES, NUM_STRIPS, Shared};

struct Counting;
thread_local! {
    // Only the test's own thread is counted: the test harness's main
    // thread may allocate while it waits.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
fn armed() -> bool {
    ARMED.try_with(Cell::get).unwrap_or(false)
}
static EVENTS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards unchanged to `System`, a correct allocator; counting does not allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if armed() {
            EVENTS.fetch_add(1, SeqCst);
        }
        // SAFETY: forwards the caller's layout unchanged to the system allocator.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if armed() {
            EVENTS.fetch_add(1, SeqCst);
        }
        // SAFETY: `p` was allocated by `System` (alloc/realloc forward there) with layout `l`.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if armed() {
            EVENTS.fetch_add(1, SeqCst);
        }
        // SAFETY: `p` was allocated by `System` with layout `l`; the arguments are forwarded unchanged.
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn process_never_allocates() {
    let sh = Shared::new();
    let mut eng = Engine::new(sh.clone());
    let input = vec![0.25f32; 4096];
    let mut outs_store = vec![vec![0.0f32; 4096]; NUM_BUSES * 2];

    ARMED.set(true);
    for cycle in 0..5000usize {
        let n = [64usize, 128, 256, 4096, 1][cycle % 5];
        // mutate parameters like the UI would
        let s = cycle % NUM_STRIPS;
        sh.strips[s].gain_db.store(-((cycle % 60) as f32));
        sh.strips[s].mute.store(cycle % 7 == 0, SeqCst);
        sh.strips[s].solo.store(cycle % 11 == 0, SeqCst);
        sh.strips[s].mono.store(cycle % 3 == 0, SeqCst);
        sh.strips[s].routes.store((cycle % 32) as u8, SeqCst);
        sh.buses[cycle % NUM_BUSES].mute.store(cycle % 5 == 0, SeqCst);
        // bus sends, loops included (the engine must keep them open), and
        // the server changing its sample rate under the EQs
        sh.buses[cycle % NUM_BUSES].routes.store((cycle.wrapping_mul(13) % 32) as u8, SeqCst);
        if cycle % 250 == 0 {
            sh.sample_rate.store([44_100, 48_000, 96_000, 0][cycle / 250 % 4], SeqCst);
            sh.strips[s].eq.on.store(true, SeqCst);
            sh.buses[cycle % NUM_BUSES].eq.on.store(true, SeqCst);
        }
        for st in 0..NUM_STRIPS {
            eng.load_input(st, 0, &input[..n]);
            eng.clear_input(st, 1, n);
        }
        let mut it = outs_store.iter_mut();
        let mut outs: [[&mut [f32]; 2]; NUM_BUSES] = std::array::from_fn(|_| {
            let l = &mut it.next().unwrap()[..n];
            let r = &mut it.next().unwrap()[..n];
            [l, r]
        });
        eng.process(&mut outs);
        let _ = (sh.strips[s].peak[0].take(), sh.strips[s].input_peak[1].take());
        assert!(outs.iter().flatten().all(|ch| ch.iter().all(|x| x.is_finite() && x.abs() < 1e6)), "bounded at cycle {cycle}");
    }
    ARMED.set(false);
    assert_eq!(EVENTS.load(SeqCst), 0, "heap was touched in the real-time path");
}
