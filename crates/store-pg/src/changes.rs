//! Lossy invalidations. Store reads and fallback timers remain authoritative.
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgListener};
use tokio::sync::{broadcast, mpsc, watch};
use uuid::Uuid;

const CHANNEL: &str = "lightspeed_changes";
const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
const MAX_PAYLOAD: usize = 7_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Session,
    Bot,
    Environment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    #[serde(rename = "u")]
    pub universe: Uuid,
    #[serde(rename = "k")]
    pub kind: ChangeKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
}

impl Change {
    pub fn new(universe: Uuid, kind: ChangeKind, id: impl Into<String>) -> Self {
        Self {
            universe,
            kind,
            id: id.into(),
            seq: None,
            rev: None,
        }
    }

    fn key(&self) -> (Uuid, ChangeKind, String) {
        (self.universe, self.kind, self.id.clone())
    }
}

#[derive(Serialize, Deserialize)]
struct Batch {
    v: u8,
    changes: Vec<Change>,
}

type Key = (Uuid, ChangeKind, String);

/// `None` on the broadcast feed means every subscriber must re-read its state.
/// A lagged broadcast receiver must do the same. Row subscriptions retain a
/// wakeup between the caller's read and wait, without waking unrelated rows.
pub struct ChangeBus {
    rows: Mutex<BTreeMap<Key, watch::Sender<u64>>>,
    all: broadcast::Sender<Option<Change>>,
    outgoing: Option<mpsc::Sender<Change>>,
    publish_failures: Arc<AtomicU64>,
}

impl Default for ChangeBus {
    fn default() -> Self {
        Self {
            rows: Mutex::new(BTreeMap::new()),
            all: broadcast::channel(1024).0,
            outgoing: None,
            publish_failures: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl ChangeBus {
    /// Start one publisher and listener for the deployment. Acquiring through
    /// the existing pool also picks up refreshed workload-identity credentials.
    pub fn postgres(pool: PgPool) -> Arc<Self> {
        let (outgoing, receiver) = mpsc::channel(4096);
        let bus = Arc::new(Self {
            outgoing: Some(outgoing),
            ..Self::default()
        });
        tokio::spawn(publish_loop(
            pool.clone(),
            receiver,
            bus.publish_failures.clone(),
        ));
        tokio::spawn(listen_loop(pool, bus.clone()));
        bus
    }

    pub fn subscribe(&self, universe: Uuid, kind: ChangeKind, id: &str) -> watch::Receiver<u64> {
        let mut rows = self
            .rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rows.retain(|_, sender| sender.receiver_count() > 0);
        rows.entry((universe, kind, id.to_owned()))
            .or_insert_with(|| watch::channel(0).0)
            .subscribe()
    }

    pub fn publish_failures(&self) -> u64 {
        self.publish_failures.load(Ordering::Relaxed)
    }

    pub fn subscribe_all(&self) -> broadcast::Receiver<Option<Change>> {
        self.all.subscribe()
    }

    /// Call only after the write commits. Never waits for a connection or a
    /// consumer: a failed/full publisher cannot delay or fail the durable write.
    pub fn publish(&self, change: Change) {
        self.deliver(change.clone());
        if let Some(outgoing) = &self.outgoing
            && outgoing.try_send(change).is_err()
        {
            self.publish_failures.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                event = "change_publish_dropped",
                "change publisher unavailable or full"
            );
        }
    }

    fn deliver(&self, change: Change) {
        let rows = self
            .rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sender) = rows.get(&change.key()) {
            sender.send_modify(|version| *version = version.wrapping_add(1));
        }
        let _ = self.all.send(Some(change));
    }

    pub fn reset(&self) {
        let rows = self
            .rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for sender in rows.values() {
            sender.send_modify(|version| *version = version.wrapping_add(1));
        }
        let _ = self.all.send(None);
    }

    fn receive(&self, payload: &str) {
        match serde_json::from_str::<Batch>(payload) {
            Ok(batch) if batch.v == 1 => {
                for change in batch.changes {
                    self.deliver(change);
                }
            }
            _ => self.reset(),
        }
    }
}

pub async fn wait_for_change(receiver: &mut watch::Receiver<u64>, fallback: Duration) {
    tokio::select! {
        _ = receiver.changed() => {},
        _ = tokio::time::sleep(fallback) => {},
    }
}

fn merge(pending: &mut BTreeMap<Key, Change>, change: Change) {
    pending
        .entry(change.key())
        .and_modify(|old| {
            old.seq = old.seq.max(change.seq);
            old.rev = old.rev.max(change.rev);
        })
        .or_insert(change);
}

fn payloads(changes: impl IntoIterator<Item = Change>) -> Vec<String> {
    let mut result = Vec::new();
    let mut batch = Batch {
        v: 1,
        changes: Vec::new(),
    };
    for change in changes {
        batch.changes.push(change);
        if serde_json::to_vec(&batch)
            .expect("change serialization")
            .len()
            > MAX_PAYLOAD
        {
            let last = batch.changes.pop().expect("last change");
            if !batch.changes.is_empty() {
                result.push(serde_json::to_string(&batch).expect("change serialization"));
            }
            batch.changes = vec![last];
        }
    }
    if !batch.changes.is_empty() {
        result.push(serde_json::to_string(&batch).expect("change serialization"));
    }
    result
        .into_iter()
        .map(|payload| {
            if payload.len() > MAX_PAYLOAD {
                "{\"v\":0,\"changes\":[]}".to_owned()
            } else {
                payload
            }
        })
        .collect()
}

async fn publish_loop(
    pool: PgPool,
    mut receiver: mpsc::Receiver<Change>,
    failures: Arc<AtomicU64>,
) {
    loop {
        let first = tokio::select! {
            _ = pool.close_event() => return,
            change = receiver.recv() => match change { Some(change) => change, None => return },
        };
        let mut pending = BTreeMap::new();
        merge(&mut pending, first);
        let deadline = tokio::time::sleep(FLUSH_INTERVAL);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                biased;
                _ = pool.close_event() => return,
                _ = &mut deadline => break,
                change = receiver.recv() => match change { Some(change) => merge(&mut pending, change), None => break },
            }
            if pending.len() >= 4096 {
                break;
            }
        }
        for payload in payloads(pending.into_values()) {
            let result = async {
                let mut tx = pool.begin().await?;
                sqlx::query("SET LOCAL synchronous_commit = off")
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("SELECT pg_notify($1, $2)")
                    .bind(CHANNEL)
                    .bind(payload)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await
            }
            .await;
            if let Err(error) = result {
                failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%error, event = "change_publish_failed", "could not publish committed changes");
            }
        }
    }
}

async fn listen_loop(pool: PgPool, bus: Arc<ChangeBus>) {
    loop {
        let result = async {
            let mut listener = PgListener::connect_with(&pool).await?;
            listener.listen(CHANNEL).await?;
            bus.reset();
            loop {
                match listener.try_recv().await? {
                    Some(notification) => bus.receive(notification.payload()),
                    None => bus.reset(), // sqlx has reconnected and restored LISTEN.
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), sqlx::Error>(())
        }
        .await;
        if pool.is_closed() {
            return;
        }
        bus.reset();
        if let Err(error) = result {
            tracing::warn!(%error, "change listener reconnecting");
        }
        tokio::select! { _ = pool.close_event() => return, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn subscription_retains_wakeup_and_isolates_rows_and_universes() {
        let bus = ChangeBus::default();
        let universe = Uuid::new_v4();
        let mut receiver = bus.subscribe(universe, ChangeKind::Session, "one");
        let other = bus.subscribe(Uuid::new_v4(), ChangeKind::Session, "one");
        bus.publish(Change::new(universe, ChangeKind::Session, "one"));
        assert!(receiver.has_changed().unwrap());
        assert!(!other.has_changed().unwrap());
        receiver.changed().await.unwrap();
        assert!(!receiver.has_changed().unwrap());
        bus.receive("{\"v\":2,\"changes\":[]}");
        assert!(receiver.has_changed().unwrap());
        assert!(other.has_changed().unwrap());
    }

    #[test]
    fn merging_preserves_visible_revision_and_splits_by_encoded_bytes() {
        let universe = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        let mut first = Change::new(universe, ChangeKind::Session, "one");
        first.rev = Some(7);
        first.seq = Some(10);
        merge(&mut changes, first.clone());
        first.rev = None;
        first.seq = Some(11);
        merge(&mut changes, first);
        assert_eq!(changes.values().next().unwrap().rev, Some(7));
        assert_eq!(changes.values().next().unwrap().seq, Some(11));
        let batches = payloads(
            (0..200).map(|id| Change::new(universe, ChangeKind::Session, format!("日本語_{id}"))),
        );
        assert!(batches.len() > 1);
        assert!(batches.iter().all(|batch| batch.len() <= MAX_PAYLOAD));
        assert_eq!(
            batches
                .iter()
                .map(|batch| serde_json::from_str::<Batch>(batch).unwrap().changes.len())
                .sum::<usize>(),
            200
        );
    }
}
