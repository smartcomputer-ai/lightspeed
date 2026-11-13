//! Ephemeral list watches. One reader batches store changes per universe;
//! each subscriber has a bounded, coalescing queue. A reset repairs lost hints.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    convert::Infallible,
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::Duration,
};

use api::{AgentApiError, AgentNotification, ResourceAccessSummary, UniverseWatchParams, WatchKind};
use axum::response::{IntoResponse, Response, Sse, sse::{Event, KeepAlive}};
use store_pg::{PgStore, changes::{Change, ChangeKind}};
use tokio::sync::{Notify, broadcast, watch};
use uuid::Uuid;

use super::{request_context::RequestContext, service::{bot_list_item, session_summary_view}};

const BATCH_INTERVAL: Duration = Duration::from_millis(250);
const STREAM_LIFETIME: Duration = Duration::from_secs(15 * 60);
const MAX_STREAMS: usize = 1024;
const MAX_KEY_STREAMS: usize = 64;
const MAX_PENDING_IDS: usize = 2048;
const MAX_QUEUE_ROWS: usize = 256;
const MAX_QUEUE_BYTES: usize = 2 * 1024 * 1024;
const RESET: &str = "{\"method\":\"watch/reset\",\"params\":{}}";
type RowKey = (WatchKind, String);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
pub(super) struct WatchRegistry(Mutex<RegistryState>);

#[derive(Default)]
struct RegistryState {
    hubs: HashMap<Uuid, Weak<Hub>>,
    keys: BTreeMap<String, usize>,
    total: usize,
}

struct Permit {
    registry: Arc<WatchRegistry>,
    key: String,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = lock(&self.registry.0);
        state.total -= 1;
        if let Some(count) = state.keys.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 { state.keys.remove(&self.key); }
        }
    }
}

impl WatchRegistry {
    pub(super) fn subscribe(
        self: &Arc<Self>, store: Arc<PgStore>, context: &RequestContext, params: UniverseWatchParams,
    ) -> Result<Subscription, AgentApiError> {
        validate(context, &params)?;
        let key = context.key.as_ref().map_or_else(|| "local".to_owned(), |key|
            format!("{:?}/{}", key.scope, key.key_id));
        let mut state = lock(&self.0);
        if state.total >= MAX_STREAMS || state.keys.get(&key).copied().unwrap_or(0) >= MAX_KEY_STREAMS {
            return Err(AgentApiError::rejected("watch connection limit reached"));
        }
        let universe = store.config().universe_id;
        state.hubs.retain(|_, hub| hub.strong_count() > 0);
        let queue = Arc::new(Queue::new());
        let subscriber = Subscriber { kinds: params.kinds.into_iter().collect(), visible_to: params.visible_to, queue: queue.clone() };
        let mut hub = state.hubs.get(&universe).and_then(Weak::upgrade);
        let mut id = hub.as_ref().and_then(|hub| hub.add(subscriber.clone()));
        if id.is_none() {
            let fresh = Arc::new(Hub { store, subscribers: Mutex::new(Subscribers::default()), wake: Notify::new() });
            id = fresh.add(subscriber);
            // Subscribe synchronously, before the initial reset can reach a client.
            let changes = fresh.store.changes().subscribe_all();
            tokio::spawn(run_hub(fresh.clone(), changes));
            state.hubs.insert(universe, Arc::downgrade(&fresh));
            hub = Some(fresh);
        }
        state.total += 1;
        *state.keys.entry(key.clone()).or_default() += 1;
        let receiver = queue.wake.subscribe();
        Ok(Subscription { hub: hub.expect("subscribed hub"), id: id.expect("subscriber id"), queue, receiver,
            _permit: Permit { registry: self.clone(), key } })
    }
}

fn validate(context: &RequestContext, params: &UniverseWatchParams) -> Result<(), AgentApiError> {
    if params.kinds.is_empty() || params.kinds.len() > 3 {
        return Err(AgentApiError::invalid_request("watch kinds must contain one to three collections"));
    }
    if params.visible_to.as_ref().is_some_and(|actor| !super::authentication::valid_actor(actor)) {
        return Err(AgentApiError::invalid_request("invalid visibleTo actor"));
    }
    if context.key.as_ref().is_some_and(|key| params.kinds.iter().any(|kind| !key.groups.contains(&kind.group()))) {
        return Err(AgentApiError::forbidden());
    }
    Ok(())
}

fn visible(access: &ResourceAccessSummary, actor: Option<&str>) -> bool {
    actor.is_none_or(|actor| access.visibility == api::Visibility::Universe ||
        matches!(&access.created_by, Some(api::Attribution::Actor { id }) if id == actor))
}

struct Hub {
    store: Arc<PgStore>,
    subscribers: Mutex<Subscribers>,
    wake: Notify,
}

#[derive(Default)]
struct Subscribers { closed: bool, next: u64, items: BTreeMap<u64, Subscriber> }

#[derive(Clone)]
struct Subscriber { kinds: BTreeSet<WatchKind>, visible_to: Option<String>, queue: Arc<Queue> }

impl Hub {
    fn add(&self, subscriber: Subscriber) -> Option<u64> {
        let mut state = lock(&self.subscribers);
        if state.closed { return None; }
        state.next += 1;
        let id = state.next;
        state.items.insert(id, subscriber);
        Some(id)
    }

    fn close_if_empty(&self) -> bool {
        let mut state = lock(&self.subscribers);
        if state.items.is_empty() { state.closed = true; }
        state.closed
    }

    fn reset(&self) {
        for subscriber in lock(&self.subscribers).items.values() { subscriber.queue.reset(); }
    }

    fn deliver(&self, update: Update) {
        for subscriber in lock(&self.subscribers).items.values() {
            if !subscriber.kinds.contains(&update.key.0) { continue; }
            if update.key.0 == WatchKind::Session {
                match &update.access {
                    Some(access) if !visible(access, subscriber.visible_to.as_deref()) => continue,
                    // Deleted rows have no remaining access record. Never reveal a
                    // private id to a restricted audience; refetch its filtered lists.
                    None if subscriber.visible_to.is_some() => { subscriber.queue.reset(); continue; }
                    _ => {}
                }
            }
            subscriber.queue.push(update.key.clone(), update.data.clone());
        }
    }
}

struct Queue { pending: Mutex<Pending>, wake: watch::Sender<u64> }
struct Pending { reset: bool, rows: BTreeMap<RowKey, Arc<str>>, bytes: usize }

impl Queue {
    fn new() -> Self {
        Self { pending: Mutex::new(Pending { reset: true, rows: BTreeMap::new(), bytes: 0 }), wake: watch::channel(0).0 }
    }

    fn reset(&self) {
        let mut pending = lock(&self.pending);
        pending.reset = true; pending.rows.clear(); pending.bytes = 0;
        self.wake.send_modify(|version| *version = version.wrapping_add(1));
    }

    fn push(&self, key: RowKey, data: Arc<str>) {
        let mut pending = lock(&self.pending);
        if let Some(old) = pending.rows.insert(key, data.clone()) { pending.bytes -= old.len(); }
        pending.bytes += data.len();
        if pending.rows.len() > MAX_QUEUE_ROWS || pending.bytes > MAX_QUEUE_BYTES {
            pending.rows.clear(); pending.bytes = 0; pending.reset = true;
        }
        self.wake.send_modify(|version| *version = version.wrapping_add(1));
    }

    fn take(&self) -> Option<Arc<str>> {
        let mut pending = lock(&self.pending);
        if pending.reset { pending.reset = false; return Some(Arc::from(RESET)); }
        pending.rows.pop_first().map(|(_, data)| { pending.bytes -= data.len(); data })
    }
}

pub(super) struct Subscription {
    hub: Arc<Hub>, id: u64, queue: Arc<Queue>, receiver: watch::Receiver<u64>, _permit: Permit,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        lock(&self.hub.subscribers).items.remove(&self.id);
        self.hub.wake.notify_one();
    }
}

impl Subscription {
    async fn next(&mut self) -> Arc<str> {
        loop {
            if let Some(data) = self.queue.take() { return data; }
            let _ = self.receiver.changed().await;
        }
    }

    pub(super) fn response(self, id: api::RequestId) -> Response {
        self.response_for(id, STREAM_LIFETIME)
    }

    fn response_for(self, id: api::RequestId, lifetime: Duration) -> Response {
        let deadline = tokio::time::Instant::now() + lifetime;
        let stream = futures_util::stream::unfold(Some(self), move |state| {
            let id = id.clone();
            async move {
                let mut subscription = state?;
                let next_message = if tokio::time::Instant::now() >= deadline { None } else { tokio::time::timeout_at(deadline, subscription.next()).await.ok() };
                let (data, next) = match next_message {
                    Some(data) => (data.to_string(), Some(subscription)),
                    None => (serde_json::to_string(&api::JsonRpcResponse::success(id,
                        api::AgentApiOutcome::new(api::UniverseWatchResponse {}))).expect("watch response serializes"), None),
                };
                Some((Ok::<_, Infallible>(Event::default().data(data)), next))
            }
        });
        let mut response = Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response();
        response.headers_mut().insert("cache-control", "no-store".parse().expect("static header"));
        response.headers_mut().insert("x-accel-buffering", "no".parse().expect("static header"));
        response
    }
}

fn row_key(change: Change) -> Option<RowKey> {
    let kind = match change.kind {
        ChangeKind::Session if change.rev.is_none() => return None,
        ChangeKind::Session => WatchKind::Session,
        ChangeKind::Bot => WatchKind::Bot,
        ChangeKind::Environment => WatchKind::Environment,
    };
    Some((kind, change.id))
}

async fn run_hub(hub: Arc<Hub>, mut changes: broadcast::Receiver<Option<Change>>) {
    let mut pending = BTreeSet::new();
    let mut tick = tokio::time::interval(BATCH_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;
    loop {
        if hub.close_if_empty() { break; }
        tokio::select! {
            _ = hub.wake.notified() => {},
            change = changes.recv() => match change {
                Ok(Some(change)) if change.universe == hub.store.config().universe_id => {
                    if let Some(key) = row_key(change) { pending.insert(key); }
                    if pending.len() > MAX_PENDING_IDS { pending.clear(); hub.reset(); }
                }
                Ok(Some(_)) => {},
                Ok(None) | Err(broadcast::error::RecvError::Lagged(_)) => { pending.clear(); hub.reset(); },
                Err(broadcast::error::RecvError::Closed) => { hub.reset(); break; },
            },
            _ = tick.tick(), if !pending.is_empty() => {
                let ids = std::mem::take(&mut pending);
                match load_changes(&hub.store, &ids).await {
                    Ok(updates) => for update in updates { hub.deliver(update); },
                    Err(error) => { tracing::warn!(%error, "watch batch read failed"); hub.reset(); },
                }
            },
        }
    }
}

struct Update { key: RowKey, access: Option<ResourceAccessSummary>, data: Arc<str> }

fn update(key: RowKey, access: Option<ResourceAccessSummary>, notification: AgentNotification) -> Result<Update, AgentApiError> {
    let data = serde_json::to_string(&notification).map_err(|e| AgentApiError::internal(e.to_string()))?;
    Ok(Update { key, access, data: Arc::from(data) })
}

async fn load_changes(store: &PgStore, keys: &BTreeSet<RowKey>) -> Result<Vec<Update>, AgentApiError> {
    let ids = |kind| keys.iter().filter(|(k, _)| *k == kind).map(|(_, id)| id.clone()).collect::<Vec<_>>();
    let mut missing = keys.clone();
    let mut updates = Vec::new();
    let sessions = ids(WatchKind::Session);
    if !sessions.is_empty() {
        for (record, access) in store.session_summaries_by_ids(&sessions).await.map_err(|e| AgentApiError::internal(e.to_string()))? {
            let key = (WatchKind::Session, record.session_id.to_string());
            let session = session_summary_view(record.clone(), &record, access.clone());
            missing.remove(&key);
            updates.push(update(key, Some(access), AgentNotification::SessionChanged { session: Box::new(session) })?);
        }
    }
    let bots = ids(WatchKind::Bot);
    if !bots.is_empty() {
        for (row, access) in store.bot_roster_by_ids(None, Some(&bots)).await.map_err(|e| AgentApiError::internal(e.to_string()))? {
            let key = (WatchKind::Bot, row.bot.bot_id.to_string());
            missing.remove(&key);
            updates.push(update(key, None, AgentNotification::BotChanged { bot: Box::new(bot_list_item(row, access)) })?);
        }
    }
    let environments = ids(WatchKind::Environment);
    if !environments.is_empty() {
        for record in store.environments_by_ids(&environments).await.map_err(|e| AgentApiError::internal(e.to_string()))? {
            let key = (WatchKind::Environment, record.environment_id.to_string());
            missing.remove(&key);
            updates.push(update(key, None, AgentNotification::EnvironmentChanged {
                environment: Box::new(crate::environments::providers::environment_view(&record)),
            })?);
        }
    }
    for key in missing {
        let notification = match key.0 {
            WatchKind::Session => AgentNotification::SessionRemoved { session_id: key.1.clone() },
            WatchKind::Bot => AgentNotification::BotRemoved { bot_id: key.1.clone() },
            WatchKind::Environment => AgentNotification::EnvironmentRemoved { environment_id: key.1.clone() },
        };
        updates.push(update(key, None, notification)?);
    }
    Ok(updates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{AccessScope, Attribution, MethodGroup, Visibility};
    use crate::gateway::request_context::KeyContext;

    fn context() -> RequestContext {
        RequestContext::local(AccessScope::Universe { universe_id: Uuid::nil() })
    }

    fn params() -> UniverseWatchParams {
        UniverseWatchParams { kinds: WatchKind::all(), visible_to: None }
    }

    #[test]
    fn each_requested_collection_requires_its_group() {
        let mut caller = context();
        caller.key = Some(KeyContext { scope: caller.scope, key_id: "reader".into(), prefix: "test".into(), groups: BTreeSet::from([MethodGroup::Session]) });
        assert_eq!(validate(&caller, &params()).unwrap_err().kind, api::AgentApiErrorKind::Forbidden);
        let mut selected = params(); selected.kinds = vec![WatchKind::Session];
        assert!(validate(&caller, &selected).is_ok());
        selected.kinds.clear();
        assert_eq!(validate(&caller, &selected).unwrap_err().kind, api::AgentApiErrorKind::InvalidRequest);
        selected = params(); selected.visible_to = Some(" ".into());
        assert!(validate(&context(), &selected).is_err());
    }

    #[test]
    fn visibility_uses_the_actor_attribution_and_root_sharing() {
        for (visibility, created_by, expected) in [
            (Visibility::Universe, None, true),
            (Visibility::Restricted, None, false),
            (Visibility::Restricted, Some(Attribution::Actor { id: "alice".into() }), true),
            (Visibility::Restricted, Some(Attribution::Actor { id: "bob".into() }), false),
            (Visibility::Restricted, Some(Attribution::Local), false),
        ] {
            let access = ResourceAccessSummary { visibility, created_by };
            assert_eq!(visible(&access, Some("alice")), expected);
            assert!(visible(&access, None));
        }
    }

    #[test]
    fn log_only_appends_do_not_dirty_list_rows() {
        let mut change = Change::new(Uuid::nil(), ChangeKind::Session, "session");
        change.seq = Some(5);
        assert!(row_key(change.clone()).is_none());
        change.rev = Some(2);
        assert_eq!(row_key(change), Some((WatchKind::Session, "session".into())));
    }

    #[test]
    fn queue_coalesces_and_overflow_replaces_pending_rows_with_reset() {
        let queue = Queue::new();
        assert_eq!(&*queue.take().unwrap(), RESET);
        let key = (WatchKind::Bot, "bot".into());
        queue.push(key.clone(), Arc::from("old"));
        queue.push(key, Arc::from("new"));
        assert_eq!(&*queue.take().unwrap(), "new");
        assert!(queue.take().is_none());
        for id in 0..=MAX_QUEUE_ROWS { queue.push((WatchKind::Bot, id.to_string()), Arc::from("data")); }
        assert_eq!(&*queue.take().unwrap(), RESET);
        assert!(queue.take().is_none());
        queue.push((WatchKind::Bot, "large".into()), Arc::from("x".repeat(MAX_QUEUE_BYTES + 1)));
        assert_eq!(&*queue.take().unwrap(), RESET);
        assert!(queue.take().is_none());
    }

    fn store() -> Arc<PgStore> {
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://localhost/unused").unwrap();
        Arc::new(PgStore::new(pool, store_pg::PgStoreConfig::new(Uuid::nil())))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribers_share_a_hub_and_drop_releases_limits() {
        let registry = Arc::new(WatchRegistry::default());
        let store = store();
        let mut subscriptions = Vec::new();
        for _ in 0..MAX_KEY_STREAMS { subscriptions.push(registry.subscribe(store.clone(), &context(), params()).unwrap()); }
        assert!(Arc::ptr_eq(&subscriptions[0].hub, &subscriptions[1].hub));
        assert!(registry.subscribe(store.clone(), &context(), params()).is_err());
        drop(subscriptions);
        assert_eq!(lock(&registry.0).total, 0);
        assert!(registry.subscribe(store, &context(), params()).is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restricted_removals_reset_without_disclosing_ids_and_other_kinds_are_filtered() {
        let registry = Arc::new(WatchRegistry::default());
        let mut params = params(); params.visible_to = Some("alice".into()); params.kinds = vec![WatchKind::Session];
        let subscription = registry.subscribe(store(), &context(), params).unwrap();
        subscription.queue.take();
        subscription.hub.deliver(update((WatchKind::Bot, "bot".into()), None,
            AgentNotification::BotRemoved { bot_id: "bot".into() }).unwrap());
        assert!(subscription.queue.take().is_none());
        subscription.hub.deliver(update((WatchKind::Session, "private".into()), None,
            AgentNotification::SessionRemoved { session_id: "private".into() }).unwrap());
        assert_eq!(&*subscription.queue.take().unwrap(), RESET);
        assert!(subscription.queue.take().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_starts_with_reset_and_ends_with_response_and_releases_subscription() {
        let registry = Arc::new(WatchRegistry::default());
        let subscription = registry.subscribe(store(), &context(), params()).unwrap();
        let response = subscription.response_for(api::RequestId::Number(7), Duration::from_millis(5));
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let bytes = tokio::time::timeout(Duration::from_secs(1), axum::body::to_bytes(response.into_body(), 4096)).await.unwrap().unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let events: Vec<serde_json::Value> = text.lines().filter_map(|line| line.strip_prefix("data: "))
            .map(|data| serde_json::from_str(data).unwrap()).collect();
        assert_eq!(events[0]["method"], "watch/reset");
        assert_eq!(events[1]["id"], 7);
        assert_eq!(events[1]["result"]["result"], serde_json::json!({}));
        assert_eq!(lock(&registry.0).total, 0);
    }
}
