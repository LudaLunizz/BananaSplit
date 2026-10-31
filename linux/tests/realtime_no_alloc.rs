//! Proves the audio callback path never touches the heap: a counting global
//! allocator wraps the system allocator and we assert zero allocations and
//! zero frees across thousands of process() cycles with parameters changing
//! underneath (as the UI thread does).

use bananasplit::engine::{Engine, NUM_BUSES, NUM_STRIPS, Shared};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

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

// SAFETY: forwards to System unchanged, so the GlobalAlloc contract is System's; counting
// only touches an atomic and a const thread-local Cell, neither of which allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if armed() {
            EVENTS.fetch_add(1, SeqCst);
        }
        // SAFETY: forwarded with the caller's arguments, which meet System's same contract.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if armed() {
            EVENTS.fetch_add(1, SeqCst);
        }
        // SAFETY: forwarded with the caller's arguments, which meet System's same contract.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if armed() {
            EVENTS.fetch_add(1, SeqCst);
        }
        // SAFETY: forwarded with the caller's arguments, which meet System's same contract.
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
        // gate and compressor switching and moving (opening, closing, gliding)
        let d = &sh.strips[s].dynamics;
        d.gate_on.store(cycle % 4 != 0, SeqCst);
        d.comp_on.store(cycle % 6 != 1, SeqCst);
        d.gate[0].store(-((cycle % 70) as f32));
        d.comp[1].store(1.0 + (cycle % 19) as f32);
        d.comp[5].store((cycle % 12) as f32);
        let _ = (d.gate_gr.load(), d.comp_gr.load());
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
        assert!(
            outs.iter().flatten().all(|ch| ch.iter().all(|x| x.is_finite() && x.abs() < 1e6)),
            "bounded at cycle {cycle}"
        );
    }
    ARMED.set(false);
    assert_eq!(EVENTS.load(SeqCst), 0, "heap was touched in the real-time path");
}
