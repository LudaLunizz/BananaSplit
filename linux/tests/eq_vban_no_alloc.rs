//! Same guarantee as `realtime_no_alloc.rs`, with every EQ working (bands
//! boosting, changing type, frequency, gain and Q, switching on and off) and
//! VBAN streams flowing in and out (including underruns, a full outgoing
//! ring and flush requests). A counting global allocator asserts that the
//! audio path never touches the heap.
//!
//! (Separate test binary with its own counting allocator, which counts
//! the test's own thread.)

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use bananasplit::engine::{Engine, NUM_BUSES, NUM_STRIPS, Shared};
use bananasplit::eq::{BANDS, Kind};
use bananasplit::vban::VbanTap;

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
fn eq_and_vban_never_allocate() {
    let sh = Shared::new();
    let tap = VbanTap::new();
    let mut eng = Engine::new(sh.clone());
    eng.set_sample_rate(48_000);
    eng.attach_vban(tap.clone());
    for ch in 0..NUM_STRIPS + NUM_BUSES {
        sh.eq(ch).on.store(true, SeqCst);
    }
    for k in 0..4 {
        tap.test_in(k, k % NUM_STRIPS, 300);
        tap.test_out(k, [0, 3, 6, 9][k]);
    }
    let input = vec![0.25f32; 4096];
    let net: Vec<f32> = (0..2 * 512).map(|i| ((i % 97) as f32 / 97.0) - 0.5).collect();
    let mut outs_store = vec![vec![0.0f32; 4096]; NUM_BUSES * 2];
    let mut sent = 0usize;

    ARMED.set(true);
    for cycle in 0..5000usize {
        let n = [64usize, 128, 256, 4096, 1][cycle % 5];
        let s = cycle % NUM_STRIPS;
        sh.strips[s].gain_db.store(-((cycle % 60) as f32));
        sh.strips[s].mute.store(cycle % 7 == 0, SeqCst);
        sh.strips[s].routes.store((cycle % 32) as u8, SeqCst);
        // EQ: move every parameter of some band of some channel
        let ch = cycle % (NUM_STRIPS + NUM_BUSES);
        let b = &sh.eq(ch).bands[cycle % BANDS];
        b.gain_db.store(((cycle % 49) as f32) - 24.0);
        b.freq.store(20.0 * 1.5f32.powi((cycle % 17) as i32));
        b.q.store(0.1 + (cycle % 30) as f32);
        b.kind.store(Kind::ALL[cycle % Kind::ALL.len()] as u8, SeqCst);
        b.on.store(cycle % 13 != 0, SeqCst);
        sh.eq((ch + 3) % 11).on.store(cycle % 17 != 0, SeqCst);
        // VBAN: the "network" sometimes keeps up and sometimes does not
        for k in 0..4 {
            if cycle % (k + 2) != 0 {
                tap.test_push_in(k, &net[..2 * [128, 256, 512, 64][k]]);
            }
        }
        if cycle % 500 == 250 {
            tap.test_flush_in(cycle / 500 % 4);
        }
        if cycle % 3 != 0 {
            for k in 0..4 {
                sent += tap.test_drain_out(k);
            }
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
        assert!(outs.iter().all(|b| b[0].iter().chain(b[1].iter()).all(|v| v.is_finite())));
        let _ = sh.strips[s].peak[0].take();
    }
    ARMED.set(false);
    assert_eq!(EVENTS.load(SeqCst), 0, "heap was touched in the real-time path");
    assert!(sent > 100_000, "outgoing streams were fed ({sent} samples)");
}
