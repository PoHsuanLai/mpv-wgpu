//! The host wake closure and the small lock helper the cores share.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

const WAKE_IDLE: u8 = 0;
const WAKE_PENDING: u8 = 1;

/// A wake the core raises from its own threads, and the closure it calls.
pub(crate) struct Notify {
    wake: AtomicU8,
    callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Notify {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            wake: AtomicU8::new(WAKE_IDLE),
            callback: Mutex::new(None),
        })
    }

    pub(crate) fn signal(&self) {
        self.wake.store(WAKE_PENDING, Ordering::Release);
        let callback = lock(&self.callback).clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// Forget a pending wake: the host is about to drain.
    pub(crate) fn clear_pending(&self) {
        self.wake.store(WAKE_IDLE, Ordering::Release);
    }

    pub(crate) fn set(&self, notify: Option<Arc<dyn Fn() + Send + Sync>>) {
        *lock(&self.callback) = notify;
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}
