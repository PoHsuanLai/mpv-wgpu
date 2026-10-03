//! Counters for the once-a-second log line.

use std::time::{Duration, Instant};

pub(crate) struct Stats {
    pub(crate) frames: u64,
    pub(crate) repeats: u64,
    pub(crate) bytes: u64,
    pub(crate) software: Duration,
    pub(crate) upload: Duration,
    pub(crate) window: Instant,
}

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            frames: 0,
            repeats: 0,
            bytes: 0,
            software: Duration::ZERO,
            upload: Duration::ZERO,
            window: Instant::now(),
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }
}
