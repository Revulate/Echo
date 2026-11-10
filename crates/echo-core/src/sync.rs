//! Poison-tolerant extensions for standard-library synchronization primitives.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Extension trait for [`RwLock`] with graceful poison recovery.
pub trait RwLockExt<T> {
    fn read_or_recover(&self) -> RwLockReadGuard<'_, T>;
    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_or_recover(&self) -> RwLockReadGuard<'_, T> {
        match self.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "RwLock was poisoned during read acquisition, recovering. \
                     This indicates a previous thread panicked while holding the lock."
                );
                poisoned.into_inner()
            }
        }
    }

    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T> {
        match self.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "RwLock was poisoned during write acquisition, recovering. \
                     This indicates a previous thread panicked while holding the lock."
                );
                poisoned.into_inner()
            }
        }
    }
}

/// Extension trait for [`Mutex`] with graceful poison recovery.
pub trait MutexExt<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        match self.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "Mutex was poisoned during lock acquisition, recovering. \
                     This indicates a previous thread panicked while holding the lock."
                );
                poisoned.into_inner()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_rwlock_read_normal() {
        let lock = RwLock::new(42);
        let guard = lock.read_or_recover();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_mutex_lock_normal() {
        let lock = Mutex::new(42);
        let guard = lock.lock_or_recover();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_rwlock_poison_recovery() {
        let lock = Arc::new(RwLock::new(42));
        let lock_clone = Arc::clone(&lock);
        let handle = thread::spawn(move || {
            let _guard = lock_clone.write().unwrap();
            panic!("Intentional panic to poison the lock");
        });
        let _ = handle.join();

        let guard = lock.read_or_recover();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_mutex_poison_recovery() {
        let lock = Arc::new(Mutex::new(42));
        let lock_clone = Arc::clone(&lock);
        let handle = thread::spawn(move || {
            let _guard = lock_clone.lock().unwrap();
            panic!("Intentional panic to poison the lock");
        });
        let _ = handle.join();

        let guard = lock.lock_or_recover();
        assert_eq!(*guard, 42);
    }
}
