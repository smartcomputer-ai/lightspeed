//! Process-local environment signals: the reconciler's work nudge and the
//! revocation of live environment routes.
//!
//! One instance per process. A process without the environment-gateway
//! role has no subscribers, so its signals reach nothing; the gateway's own
//! sweeps catch up within one sweep interval.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use tokio::sync::{Notify, watch};
use uuid::Uuid;

/// Process-local environment signals. One instance per process; processes
/// without the environment-gateway role have no subscribers, and the
/// gateway's sweeps catch up within one sweep interval.
#[derive(Default)]
pub struct EnvironmentSignals {
    work: Notify,
    routes: Mutex<HashMap<(Uuid, String), watch::Sender<u64>>>,
}

impl EnvironmentSignals {
    /// Wake the reconciler now instead of at its next tick. A nudge with no
    /// reconciler waiting is kept until one waits.
    pub fn nudge_work(&self) {
        self.work.notify_one();
    }

    /// Resolves at the next nudge.
    pub async fn work_nudged(&self) {
        self.work.notified().await;
    }

    /// Subscribe a live control connection or data route to the
    /// environment's revocation: the receiver changes on every revoke.
    pub fn watch(&self, universe_id: Uuid, environment_id: &str) -> watch::Receiver<u64> {
        let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
        routes
            .entry((universe_id, environment_id.to_owned()))
            .or_insert_with(|| watch::channel(0).0)
            .subscribe()
    }

    /// End every live route of the environment; a control connection
    /// re-reads its row and sends `registration/closed` if it is closed.
    /// No-op without subscribers.
    pub fn revoke(&self, universe_id: Uuid, environment_id: &str) {
        let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (universe_id, environment_id.to_owned());
        let Some(sender) = routes.get(&key) else {
            return;
        };
        if sender.receiver_count() == 0 {
            routes.remove(&key);
            return;
        }
        sender.send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// Keys with live subscribers; prunes senders without receivers.
    pub fn watched(&self) -> Vec<(Uuid, String)> {
        let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
        routes.retain(|_, sender| sender.receiver_count() > 0);
        routes.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn a_nudge_before_the_wait_is_kept() {
        let signals = EnvironmentSignals::default();
        signals.nudge_work();
        tokio::time::timeout(Duration::from_secs(1), signals.work_nudged())
            .await
            .expect("the stored nudge wakes the waiter");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), signals.work_nudged())
                .await
                .is_err(),
            "one nudge wakes one wait"
        );
    }

    #[test]
    fn revoke_reaches_every_subscriber_of_the_environment_only() {
        let signals = EnvironmentSignals::default();
        let universe = Uuid::new_v4();
        let first = signals.watch(universe, "environment_a");
        let second = signals.watch(universe, "environment_a");
        let other = signals.watch(universe, "environment_b");
        signals.revoke(universe, "environment_a");
        assert!(first.has_changed().unwrap());
        assert!(second.has_changed().unwrap());
        assert!(!other.has_changed().unwrap());
        // Unknown environments and other universes are no-ops.
        signals.revoke(universe, "environment_missing");
        signals.revoke(Uuid::new_v4(), "environment_b");
        assert!(!other.has_changed().unwrap());
    }

    #[test]
    fn watched_lists_live_subscriptions_and_prunes_dropped_ones() {
        let signals = EnvironmentSignals::default();
        let universe = Uuid::new_v4();
        let kept = signals.watch(universe, "environment_a");
        drop(signals.watch(universe, "environment_b"));
        assert_eq!(
            signals.watched(),
            vec![(universe, "environment_a".to_owned())]
        );
        drop(kept);
        assert!(signals.watched().is_empty());
    }
}
