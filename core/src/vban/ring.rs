//! Lock-free single-producer / single-consumer ring of `f32` samples.
//!
//! Samples are stored as `f32::to_bits` in `AtomicU32` slots: relaxed loads
//! and stores compile to plain moves, so there is no `unsafe` and no cost
//! over a plain buffer. `head` and `tail` are ever-increasing (wrapping)
//! sample counters; Release/Acquire on them publishes the slots. Neither
//! side ever allocates, locks or makes a system call.

use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicU32, AtomicUsize};

pub struct Ring {
    buf: Box<[AtomicU32]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl Ring {
    /// A ring of at least `samples` slots (rounded up to a power of two).
    pub fn new(samples: usize) -> Ring {
        let n = samples.max(2).next_power_of_two();
        Ring {
            buf: (0..n).map(|_| AtomicU32::new(0)).collect(),
            mask: n - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Samples written and not yet consumed (either side may ask).
    pub fn len(&self) -> usize {
        let t = self.tail.load(Acquire);
        self.head.load(Acquire).wrapping_sub(t)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ---- producer ----

    /// Free slots.
    pub fn free(&self) -> usize {
        self.buf.len() - self.head.load(Relaxed).wrapping_sub(self.tail.load(Acquire))
    }

    /// Write the sample `off` places after the write position (`off < free()`).
    #[inline(always)]
    pub fn write(&self, off: usize, v: f32) {
        let h = self.head.load(Relaxed);
        self.buf[h.wrapping_add(off) & self.mask].store(v.to_bits(), Relaxed);
    }

    /// Make the next `n` written samples visible to the consumer.
    #[inline]
    pub fn publish(&self, n: usize) {
        let h = self.head.load(Relaxed);
        self.head.store(h.wrapping_add(n), Release);
    }

    /// Write and publish all of `v`, or nothing if it does not fit.
    pub fn push(&self, v: &[f32]) -> bool {
        if v.len() > self.free() {
            return false;
        }
        for (i, &x) in v.iter().enumerate() {
            self.write(i, x);
        }
        self.publish(v.len());
        true
    }

    // ---- consumer ----

    /// Readable samples.
    pub fn available(&self) -> usize {
        let t = self.tail.load(Relaxed);
        self.head.load(Acquire).wrapping_sub(t)
    }

    /// The sample `off` places after the read position (`off < available()`).
    #[inline(always)]
    pub fn read(&self, off: usize) -> f32 {
        let t = self.tail.load(Relaxed);
        f32::from_bits(self.buf[t.wrapping_add(off) & self.mask].load(Relaxed))
    }

    /// Release `n` samples (`n <= available()`).
    #[inline]
    pub fn consume(&self, n: usize) {
        let t = self.tail.load(Relaxed);
        self.tail.store(t.wrapping_add(n), Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fills_wraps_and_never_overfills() {
        let r = Ring::new(100);
        assert_eq!(r.capacity(), 128);
        let mut next = 0u32; // next value to write
        let mut expect = 0u32; // next value to read
        for round in 0..50usize {
            let len = 7 + round % 30;
            loop {
                let chunk: Vec<f32> = (0..len).map(|i| (next + i as u32) as f32).collect();
                let fits = chunk.len() <= r.free();
                assert_eq!(r.push(&chunk), fits);
                if !fits {
                    break;
                }
                next += len as u32;
            }
            assert!(r.len() <= r.capacity());
            assert_eq!(r.free() + r.available(), r.capacity());
            assert_eq!(r.available() as u32, next - expect);
            let take = r.available() / 2 + 1;
            for i in 0..take {
                assert_eq!(r.read(i), (expect + i as u32) as f32);
            }
            r.consume(take);
            expect += take as u32;
        }
    }

    #[test]
    fn two_threads() {
        let r = Arc::new(Ring::new(1024));
        let p = r.clone();
        let total = 1_000_000u32;
        let t = std::thread::spawn(move || {
            let mut i = 0u32;
            while i < total {
                let n = (total - i).min(37);
                let v: Vec<f32> = (i..i + n).map(|x| x as f32).collect();
                if p.push(&v) {
                    i += n;
                } else {
                    std::hint::spin_loop();
                }
            }
        });
        let mut seen = 0u32;
        while seen < total {
            let a = r.available();
            for k in 0..a {
                assert_eq!(r.read(k), (seen + k as u32) as f32);
            }
            r.consume(a);
            seen += a as u32;
        }
        t.join().unwrap();
        assert!(r.is_empty());
    }
}
