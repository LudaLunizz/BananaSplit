//! The rings between the devices' IO threads and the mixer never touch the
//! heap once made: pushing, the drift-correcting reads and the conversion
//! between rates (all run in Core Audio IOProcs).

use bananasplit::ring::{RateReader, Reader, ring};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

struct Counting;
thread_local! {
    // Only the test's own thread is counted.
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
fn rings_and_readers_never_allocate() {
    // a 44.1 kHz input read by the 48 kHz mixer, the mixer into a 96 kHz
    // output, a device at the mixer's rate, and a converter on one thread
    let (mut p1, c1) = ring(48_000);
    let (mut p2, c2) = ring(48_000);
    let (mut p3, c3) = ring(48_000);
    let (mut p4, c4) = ring(8192 * 9);
    let mut r1 = Reader::new(c1, 600, 44_100, 48_000);
    let mut r2 = Reader::new(c2, 600, 48_000, 96_000);
    let mut r3 = Reader::new(c3, 600, 48_000, 48_000);
    let mut r4 = RateReader::fixed(c4, 48_000, 44_100);
    let block: Vec<f32> = (0..2 * 512).map(|i| (i as f32 * 0.01).sin()).collect();
    let (mut l, mut r) = (vec![0.0f32; 8192], vec![0.0f32; 8192]);
    let mix = vec![0.25f32; 8192];

    ARMED.set(true);
    for cycle in 0..3000usize {
        let n = [64usize, 128, 441, 512, 1, 1024][cycle % 6];
        p1.push_interleaved(&block[..2 * n.min(512)]);
        p2.push_planar(&mix[..n], &mix[..n]);
        p3.push_silence(n);
        for rd in [&mut r1, &mut r2, &mut r3] {
            rd.read(&mut l[..n], &mut r[..n], cycle % 2 == 0);
            let _ = rd.glitches();
        }
        // the converter: exactly what it needs, mixed on the same thread
        let need = r4.needed(n);
        let mut have = 0;
        while have < need {
            let k = (need - have).min(8192);
            p4.push_planar(&mix[..k], &mix[..k]);
            have += k;
        }
        assert!(r4.read(&mut l[..n], &mut r[..n], false));
    }
    ARMED.set(false);
    assert_eq!(EVENTS.load(SeqCst), 0, "heap was touched in the real-time path");
}
