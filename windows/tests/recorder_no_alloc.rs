//! Same guarantee as `realtime_no_alloc.rs`, with a recorder attached and
//! recording: multi and mix sessions, live link changes, a full ring
//! (dropped blocks) and a draining consumer. A counting global allocator
//! asserts that the audio path never touches the heap.
//!
//! (Separate test binary with its own counting allocator, which counts
//! the test's own thread.)

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use bananasplit::engine::{Engine, NUM_BUSES, NUM_STRIPS, Shared};
use bananasplit::recorder::{ALL_LINKS, HEADER_WORDS, RecTap};

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
fn process_with_recorder_never_allocates() {
    let sh = Shared::new();
    let tap = RecTap::with_ring_words(1 << 20);
    let mut eng = Engine::new(sh.clone());
    eng.attach_recorder(tap.clone());
    let ring = tap.ring(); // allocated by the control side before a take starts
    let input = vec![0.25f32; 4096];
    let mut outs_store = vec![vec![0.0f32; 4096]; NUM_BUSES * 2];
    let mut dropped_total = 0u64;
    let mut blocks_seen = 0u64;

    ARMED.set(true);
    for cycle in 0..6000usize {
        if cycle % 1000 == 0 {
            dropped_total += tap.dropped_frames() as u64;
            tap.activate((cycle / 1000) as u16, cycle % 2000 == 0, ALL_LINKS);
        }
        if cycle % 1000 == 900 {
            tap.deactivate();
        }
        let n = [64usize, 128, 256, 4096, 1][cycle % 5];
        let s = cycle % NUM_STRIPS;
        sh.strips[s].gain_db.store(-((cycle % 60) as f32));
        sh.strips[s].mute.store(cycle % 7 == 0, SeqCst);
        sh.strips[s].solo.store(cycle % 11 == 0, SeqCst);
        sh.strips[s].mono.store(cycle % 3 == 0, SeqCst);
        sh.strips[s].routes.store((cycle % 32) as u8, SeqCst);
        sh.buses[cycle % NUM_BUSES].mute.store(cycle % 5 == 0, SeqCst);
        sh.rec_links.store((cycle as u16).wrapping_mul(37) & ALL_LINKS, SeqCst);
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
        let _ = sh.strips[s].peak[0].take();
        // a slow consumer: lets the ring fill up between drains
        if cycle % 200 == 0 {
            while ring.available() >= HEADER_WORDS {
                let len = HEADER_WORDS + ring.peek(1) as usize * ring.peek(3) as usize;
                ring.consume(len);
                blocks_seen += 1;
            }
        }
    }
    ARMED.set(false);
    dropped_total += tap.dropped_frames() as u64;
    assert_eq!(EVENTS.load(SeqCst), 0, "heap was touched in the real-time path");
    assert!(blocks_seen > 1000, "recorder wrote blocks ({blocks_seen})");
    assert!(dropped_total > 0, "the full-ring path was exercised");
    assert_eq!(tap.cycles(), 6000);
}
