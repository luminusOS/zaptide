use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) struct Permit<'a> {
    active: &'a AtomicUsize,
}

impl<'a> Permit<'a> {
    pub(crate) fn acquire(active: &'a AtomicUsize, limit: usize) -> Option<Self> {
        try_acquire(active, limit).then(|| Self { active })
    }
}

pub(crate) fn try_acquire(active: &AtomicUsize, limit: usize) -> bool {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < limit).then(|| count + 1)
        })
        .is_ok()
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_saturate_release_and_remain_isolated() {
        let first = AtomicUsize::new(0);
        let second = AtomicUsize::new(0);
        let one = Permit::acquire(&first, 2).unwrap();
        let two = Permit::acquire(&first, 2).unwrap();
        assert!(Permit::acquire(&first, 2).is_none());
        assert_eq!(first.load(Ordering::Acquire), 2);
        let other = Permit::acquire(&second, 2).unwrap();
        drop(one);
        let replacement = Permit::acquire(&first, 2).unwrap();
        drop((two, replacement, other));
        assert_eq!(first.load(Ordering::Acquire), 0);
        assert_eq!(second.load(Ordering::Acquire), 0);
    }

    #[test]
    fn poisoned_mutex_recovers_without_clearing_poison() {
        let mutex = Mutex::new(1);
        std::thread::scope(|scope| {
            let _ = scope
                .spawn(|| {
                    let mut guard = mutex.lock().unwrap();
                    *guard = 2;
                    panic!("poison fixture");
                })
                .join();
        });
        assert!(mutex.is_poisoned());
        *lock(&mutex) += 1;
        assert_eq!(*lock(&mutex), 3);
        assert!(mutex.is_poisoned());
    }
}
