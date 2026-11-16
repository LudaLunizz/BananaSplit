//! Two small helpers several modules need, defined once.

use std::sync::{Mutex, MutexGuard};

/// Linear ramp from `v0` to `v1` across `n` samples; sample `n-1` is exactly `v1`.
#[inline(always)]
pub(crate) fn ramp(v0: f32, v1: f32, i: usize, n: usize) -> f32 {
    if i + 1 >= n { v1 } else { v0 + (v1 - v0) * ((i + 1) as f32 / n as f32) }
}

/// Lock `m`, also when a thread panicked while holding it: the mixer keeps
/// running on whatever state was left rather than panicking the next caller.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_ends_exactly_on_the_target() {
        assert_eq!(ramp(0.0, 1.0, 3, 4), 1.0);
        assert_eq!(ramp(0.0, 1.0, 0, 4), 0.25);
        assert_eq!(ramp(2.0, 2.0, 0, 8), 2.0);
        assert_eq!(ramp(5.0, 7.0, 0, 1), 7.0, "one sample: straight to the target");
    }

    #[test]
    fn lock_survives_a_poisoned_mutex() {
        let m = std::sync::Arc::new(Mutex::new(5));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(m.lock().is_err(), "the mutex is poisoned");
        assert_eq!(*lock(&m), 5);
    }
}
