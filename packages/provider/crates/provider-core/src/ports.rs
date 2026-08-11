//! A pool of ports for remote debugging.
//!
//! Ports here are published by whatever runs the provider — a `ports:` line in
//! compose, a firewall rule — so they cannot be picked at random, and asking an
//! operator to name one per device does not survive a farm of thirty phones.
//! A range is configured once and devices take from it while exposed.

use std::collections::VecDeque;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct PortPool {
    free: Mutex<VecDeque<u16>>,
}

impl PortPool {
    pub fn new(range: RangeInclusive<u16>) -> Arc<Self> {
        Arc::new(Self {
            free: Mutex::new(range.collect()),
        })
    }

    /// Take a port, or `None` when every one is in use.
    pub fn claim(self: &Arc<Self>) -> Option<PortLease> {
        let port = self.free.lock().expect("port pool poisoned").pop_front()?;
        Some(PortLease {
            port,
            pool: Arc::clone(self),
        })
    }

    pub fn available(&self) -> usize {
        self.free.lock().expect("port pool poisoned").len()
    }
}

/// A claimed port, returned to the pool when dropped.
///
/// The lease is what the caller holds for as long as it is listening, so a
/// backend that panics or a session torn down without ceremony still gives the
/// port back — a leak here is permanent, since nothing else knows the range.
#[derive(Debug)]
pub struct PortLease {
    port: u16,
    pool: Arc<PortPool>,
}

impl PortLease {
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for PortLease {
    fn drop(&mut self) {
        // Pushed to the back so a port that just closed is the last one reused:
        // a client reconnecting to a stale port reaches a different device
        // otherwise, and adb caches its connections.
        self.pool
            .free
            .lock()
            .expect("port pool poisoned")
            .push_back(self.port);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claimed_port_comes_back_when_the_lease_drops() {
        let pool = PortPool::new(7200..=7201);
        assert_eq!(pool.available(), 2);

        let first = pool.claim().unwrap();
        let second = pool.claim().unwrap();
        assert_ne!(first.port(), second.port());
        assert_eq!(pool.available(), 0);
        assert!(pool.claim().is_none(), "an exhausted pool hands out nothing");

        let reclaimed = first.port();
        drop(first);
        assert_eq!(pool.available(), 1);
        assert_eq!(pool.claim().unwrap().port(), reclaimed);
    }

    #[test]
    fn a_single_port_range_is_a_pool_of_one() {
        let pool = PortPool::new(7200..=7200);
        let lease = pool.claim().unwrap();
        assert_eq!(lease.port(), 7200);
        assert!(pool.claim().is_none());
    }
}
