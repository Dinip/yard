//! Who currently needs a device to be streaming.
//!
//! An idle phone that nobody is watching should not be running its hardware
//! encoder. The backends cannot answer that question for themselves: video
//! subscribers are one source of demand, but HID is another, and an input event
//! is not a frame subscriber — `VideoHandle::viewer_count()` can only ever say
//! "is anyone watching", never "does anything need this device up".
//!
//! So demand is counted here, next to the video handle rather than inside it,
//! and the grace period lives here too so both backends get the same behaviour
//! from one place.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

/// How long a device keeps streaming after the last thing needing it went away.
///
/// Long enough to absorb a page refresh and the popout window's handoff, short
/// enough that the device actually cools between sessions. The right value
/// depends on what iOS media bring-up costs, which we have no measurement for
/// yet — revisit once this has run on real hardware.
pub const IDLE_GRACE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
struct State {
    leases: usize,
    /// Bumped by every [`Demand::touch`], so a producer can tell a fresh event
    /// from one it has already seen. A timestamp cannot do this: under paused
    /// time in tests two touches share an `Instant`.
    pulses: u64,
    /// When this device was last needed — the moment of the last touch, or of
    /// the last lease being dropped.
    last_active: Instant,
}

/// The demand on one device. Cheap to clone; every clone counts into the same
/// total.
#[derive(Clone)]
pub struct Demand {
    state: Arc<watch::Sender<State>>,
    /// The pulse count the producer had already accounted for when it last
    /// went idle.
    ///
    /// Held outside the watch because it is the *consumer's* bookkeeping, not
    /// part of the state being observed. Without it, an event landing in the
    /// gap between a producer tearing down and re-entering
    /// [`Demand::wait_for_demand`] would be lost — and a dropped input is the
    /// one failure this whole arrangement is not allowed to introduce.
    consumed: Arc<AtomicU64>,
}

impl Default for Demand {
    fn default() -> Self {
        Self::new()
    }
}

impl Demand {
    pub fn new() -> Self {
        let (state, _) = watch::channel(State {
            leases: 0,
            pulses: 0,
            last_active: Instant::now(),
        });
        Self {
            state: Arc::new(state),
            consumed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Claims the device for as long as the returned guard lives.
    pub fn lease(&self) -> DemandLease {
        self.state.send_modify(|state| state.leases += 1);
        DemandLease {
            state: self.state.clone(),
        }
    }

    /// Demand that arrives as events rather than as a connection.
    ///
    /// This is the input path's lease: a burst of events keeps the device up,
    /// and input stopping expires into the same grace window as a viewer
    /// leaving. Spelled as a refresh rather than a held guard because there is
    /// no event to hang the drop on — the last touch simply stops being
    /// followed by another.
    pub fn touch(&self) {
        self.state.send_modify(|state| {
            state.pulses += 1;
            state.last_active = Instant::now();
        });
    }

    /// Leases held right now. Touches are not leases and do not count here.
    pub fn leases(&self) -> usize {
        self.state.borrow().leases
    }

    /// Resolves on the edge into demand: a lease taken, or an event that the
    /// last idle window did not already account for.
    ///
    /// Demand that has already lapsed does not count, which is what keeps a
    /// producer that has just torn down from immediately building back up.
    pub async fn wait_for_demand(&self) {
        let mut changes = self.state.subscribe();
        loop {
            {
                let state = changes.borrow_and_update();
                if state.leases > 0 || state.pulses > self.consumed.load(Ordering::Relaxed) {
                    return;
                }
            }
            // The sender is held by `self`, so this cannot fail while the
            // producer is still interested.
            if changes.changed().await.is_err() {
                return;
            }
        }
    }

    /// Resolves once nothing has needed the device for `grace`.
    ///
    /// A lease taken — or an event arriving — inside the window cancels the
    /// teardown rather than deferring it, which is what makes a page refresh
    /// free.
    pub async fn wait_for_idle(&self, grace: Duration) {
        let mut changes = self.state.subscribe();
        loop {
            let (leases, last_active) = {
                let state = changes.borrow_and_update();
                (state.leases, state.last_active)
            };

            if leases > 0 {
                if changes.changed().await.is_err() {
                    return;
                }
                continue;
            }

            let deadline = last_active + grace;
            if Instant::now() >= deadline {
                // Every event so far is older than the whole window, so none of
                // them should wake the producer straight back up.
                self.consumed
                    .store(changes.borrow().pulses, Ordering::Relaxed);
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {}
                _ = changes.changed() => {}
            }
        }
    }
}

/// One claim on a device. Releases on drop, including on a panic or a dropped
/// WebSocket task — which is the whole reason this is a guard and not a pair of
/// start/stop calls.
pub struct DemandLease {
    state: Arc<watch::Sender<State>>,
}

impl Drop for DemandLease {
    fn drop(&mut self) {
        self.state.send_modify(|state| {
            state.leases = state.leases.saturating_sub(1);
            if state.leases == 0 {
                state.last_active = Instant::now();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(30);

    #[tokio::test(start_paused = true)]
    async fn a_lease_is_the_edge_into_demand() {
        let demand = Demand::new();
        let waiting = {
            let demand = demand.clone();
            tokio::spawn(async move { demand.wait_for_demand().await })
        };

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(!waiting.is_finished());

        let lease = demand.lease();
        assert_eq!(demand.leases(), 1);
        waiting.await.unwrap();
        drop(lease);
        assert_eq!(demand.leases(), 0);
    }

    /// Input is demand, and it is not a subscriber — this is the case
    /// `viewer_count()` can never represent.
    #[tokio::test(start_paused = true)]
    async fn an_event_is_demand_even_with_nobody_watching() {
        let demand = Demand::new();
        let waiting = {
            let demand = demand.clone();
            tokio::spawn(async move { demand.wait_for_demand().await })
        };

        tokio::time::advance(Duration::from_secs(1)).await;
        demand.touch();
        waiting.await.unwrap();
        assert_eq!(demand.leases(), 0);
    }

    /// A producer that has just torn down must not be woken by the demand that
    /// caused the teardown in the first place.
    #[tokio::test(start_paused = true)]
    async fn demand_that_has_already_lapsed_does_not_count() {
        let demand = Demand::new();
        demand.touch();
        drop(demand.lease());

        // The full producer cycle: the idle window is what marks that demand
        // as spent.
        demand.wait_for_idle(GRACE).await;

        let waiting = {
            let demand = demand.clone();
            tokio::spawn(async move { demand.wait_for_demand().await })
        };
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(!waiting.is_finished());
    }

    /// An event that lands while the producer is between `wait_for_idle`
    /// returning and `wait_for_demand` being entered must still wake it.
    /// Anything else silently drops the first input after an idle period.
    #[tokio::test(start_paused = true)]
    async fn an_event_in_the_gap_between_teardown_and_the_next_wait_is_not_lost() {
        let demand = Demand::new();
        demand.wait_for_idle(GRACE).await;

        demand.touch();
        tokio::time::timeout(Duration::from_secs(1), demand.wait_for_demand())
            .await
            .expect("the event should have woken the producer");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_waits_out_the_whole_grace_period() {
        let demand = Demand::new();
        let lease = demand.lease();

        let idle = {
            let demand = demand.clone();
            tokio::spawn(async move { demand.wait_for_idle(GRACE).await })
        };

        tokio::time::advance(Duration::from_secs(300)).await;
        assert!(!idle.is_finished(), "a held lease is not idle, ever");

        drop(lease);
        tokio::time::advance(GRACE - Duration::from_secs(1)).await;
        assert!(!idle.is_finished());

        tokio::time::advance(Duration::from_secs(2)).await;
        idle.await.unwrap();
    }

    /// The page-refresh case: the second viewer arrives before the first one's
    /// grace runs out, and the stream must never have stopped.
    #[tokio::test(start_paused = true)]
    async fn a_lease_inside_the_window_cancels_the_teardown() {
        let demand = Demand::new();
        let first = demand.lease();

        let idle = {
            let demand = demand.clone();
            tokio::spawn(async move { demand.wait_for_idle(GRACE).await })
        };

        drop(first);
        tokio::time::advance(GRACE / 2).await;
        let second = demand.lease();

        tokio::time::advance(GRACE * 4).await;
        assert!(
            !idle.is_finished(),
            "the reconnect should have cancelled it"
        );

        drop(second);
        tokio::time::advance(GRACE + Duration::from_secs(1)).await;
        idle.await.unwrap();
    }

    /// Input keeps the device up without ever holding a lease, and stopping is
    /// treated exactly like a viewer leaving.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_events_keeps_the_window_open() {
        let demand = Demand::new();
        demand.touch();

        let idle = {
            let demand = demand.clone();
            tokio::spawn(async move { demand.wait_for_idle(GRACE).await })
        };

        for _ in 0..10 {
            tokio::time::advance(GRACE / 2).await;
            demand.touch();
            assert!(!idle.is_finished());
        }

        tokio::time::advance(GRACE + Duration::from_secs(1)).await;
        idle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn overlapping_leases_only_go_idle_once_the_last_one_leaves() {
        let demand = Demand::new();
        let first = demand.lease();
        let second = demand.lease();
        assert_eq!(demand.leases(), 2);

        let idle = {
            let demand = demand.clone();
            tokio::spawn(async move { demand.wait_for_idle(GRACE).await })
        };

        drop(first);
        tokio::time::advance(GRACE * 2).await;
        assert!(!idle.is_finished());

        drop(second);
        tokio::time::advance(GRACE + Duration::from_secs(1)).await;
        idle.await.unwrap();
    }
}
