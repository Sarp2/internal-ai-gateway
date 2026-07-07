use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct ActiveStreamTracker {
    active_streams: AtomicUsize,
    max_active_streams: usize,
}

impl ActiveStreamTracker {
    pub fn new(max_active_streams: usize) -> Self {
        Self {
            active_streams: AtomicUsize::new(0),
            max_active_streams,
        }
    }

    pub fn current(&self) -> usize {
        self.active_streams.load(Ordering::Relaxed)
    }

    pub fn try_start_stream(&self) -> Option<ActiveStreamGuard<'_>> {
        let mut current = self.active_streams.load(Ordering::Relaxed);

        loop {
            if current >= self.max_active_streams {
                return None;
            }

            match self.active_streams.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ActiveStreamGuard {
                        tracker: self,
                        released: false,
                    });
                }
                Err(updated_current) => current = updated_current,
            }
        }
    }

    pub fn try_start_owned(self: &Arc<Self>) -> Option<OwnedActiveStreamGuard> {
        let mut current = self.active_streams.load(Ordering::Relaxed);

        loop {
            if current >= self.max_active_streams {
                return None;
            }

            match self.active_streams.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(OwnedActiveStreamGuard {
                        tracker: Arc::clone(self),
                        released: false,
                    });
                }
                Err(updated_current) => current = updated_current,
            }
        }
    }

    fn end_stream(&self) {
        self.active_streams.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct ActiveStreamGuard<'a> {
    tracker: &'a ActiveStreamTracker,
    released: bool,
}

impl ActiveStreamGuard<'_> {
    pub fn finish(mut self) {
        self.release();
    }

    fn release(&mut self) {
        if !self.released {
            self.tracker.end_stream();
            self.released = true;
        }
    }
}

impl Drop for ActiveStreamGuard<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

pub struct OwnedActiveStreamGuard {
    tracker: Arc<ActiveStreamTracker>,
    released: bool,
}

impl OwnedActiveStreamGuard {
    pub fn finish(mut self) {
        self.release();
    }

    fn release(&mut self) {
        if !self.released {
            self.tracker.end_stream();
            self.released = true;
        }
    }
}

impl Drop for OwnedActiveStreamGuard {
    fn drop(&mut self) {
        self.release();
    }
}
