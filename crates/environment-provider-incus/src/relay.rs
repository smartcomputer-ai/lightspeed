//! On-demand provider data connections.
//!
//! Lightspeed opens one provider WebSocket for a specific current target. The
//! provider validates the credential and ownership, wakes the target when it
//! is powered down, dials the target's private envd, and proxies the
//! connection until it closes or becomes idle. There is no
//! provider-initiated connection and no provider route registry in
//! Lightspeed.
//!
//! Every data connection, from the dial that may wake the target to the
//! relay's end, holds a [`RelayHold`] on its instance. The idle loop never
//! powers down or closes an instance while one is held.
//!
//! The relay is an intermediary under the protocol's liveness rule: it
//! forwards pings and pongs, counts every frame in either direction as
//! activity, and never idle-closes sooner than `relayIdleSeconds`, which
//! config loading keeps at or above the protocol minimum. Both legs carry the
//! protocol's message and frame limit.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context as _;
use axum::extract::ws::{Message as AxumMessage, WebSocket};
use environment_protocol::limits::MAX_MESSAGE_BYTES;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message as TungsteniteMessage, protocol::WebSocketConfig},
};

use crate::{Config, incus::OwnedTarget};

pub type GuestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Data connections in progress, by instance name.
#[derive(Clone, Debug, Default)]
pub struct Relays(Arc<Mutex<HashMap<String, usize>>>);

impl Relays {
    /// Count one data connection to `name` until the hold is dropped.
    pub fn hold(&self, name: &str) -> RelayHold {
        *self.lock().entry(name.to_owned()).or_default() += 1;
        RelayHold {
            relays: self.clone(),
            name: name.to_owned(),
        }
    }

    /// True while any data connection to `name` is held.
    pub fn active(&self, name: &str) -> bool {
        self.lock().get(name).is_some_and(|count| *count > 0)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One counted data connection; see [`Relays::hold`].
#[derive(Debug)]
pub struct RelayHold {
    relays: Relays,
    name: String,
}

impl Drop for RelayHold {
    fn drop(&mut self) {
        let mut relays = self.relays.lock();
        if let Some(count) = relays.get_mut(&self.name) {
            *count -= 1;
            if *count == 0 {
                relays.remove(&self.name);
            }
        }
    }
}

/// The protocol's message limit, applied to both relay legs.
pub fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES as usize))
        .max_frame_size(Some(MAX_MESSAGE_BYTES as usize))
}

pub async fn dial_guest(config: &Config, target: &OwnedTarget) -> anyhow::Result<GuestSocket> {
    dial_guest_within(
        config,
        target,
        Duration::from_secs(config.dial_timeout_seconds),
    )
    .await
}

/// Dial the guest's envd listener, giving up after `timeout`.
pub async fn dial_guest_within(
    config: &Config,
    target: &OwnedTarget,
    timeout: Duration,
) -> anyhow::Result<GuestSocket> {
    let address = target
        .ipv4_address
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("target has no private IPv4 address"))?;
    let endpoint = format!("ws://{address}:{}/", config.envd_port);
    let (socket, _) = tokio::time::timeout(
        timeout,
        connect_async_with_config(endpoint, Some(websocket_config()), true),
    )
    .await
    .context("envd dial timeout")??;
    Ok(socket)
}

pub async fn proxy(config: Config, mut lightspeed: WebSocket, mut guest: GuestSocket) {
    let idle = Duration::from_secs(config.relay_idle_seconds);
    proxy_with_idle(idle, &mut lightspeed, &mut guest).await;
}

async fn proxy_with_idle(idle: Duration, lightspeed: &mut WebSocket, guest: &mut GuestSocket) {
    loop {
        let event = tokio::time::timeout(idle, async {
            tokio::select! {
                message = lightspeed.recv() => Event::Lightspeed(message),
                message = guest.next() => Event::Guest(message),
            }
        })
        .await;
        let Ok(event) = event else {
            break;
        };
        match event {
            Event::Lightspeed(Some(Ok(message))) => {
                let close = matches!(message, AxumMessage::Close(_));
                let Some(message) = to_guest(message) else {
                    break;
                };
                if guest.send(message).await.is_err() || close {
                    break;
                }
            }
            Event::Guest(Some(Ok(message))) => {
                let close = matches!(message, TungsteniteMessage::Close(_));
                let Some(message) = to_lightspeed(message) else {
                    continue;
                };
                if lightspeed.send(message).await.is_err() || close {
                    break;
                }
            }
            Event::Lightspeed(_) | Event::Guest(None) | Event::Guest(Some(Err(_))) => break,
        }
    }
    // Always close both legs. In particular, an upstream EOF or protocol
    // error must not turn into a reset-without-close in the guest envd log.
    let _ = guest.close(None).await;
    let _ = lightspeed.send(AxumMessage::Close(None)).await;
}

enum Event {
    Lightspeed(Option<Result<AxumMessage, axum::Error>>),
    Guest(Option<Result<TungsteniteMessage, tokio_tungstenite::tungstenite::Error>>),
}

fn to_guest(message: AxumMessage) -> Option<TungsteniteMessage> {
    Some(match message {
        AxumMessage::Text(value) => TungsteniteMessage::Text(value.to_string().into()),
        AxumMessage::Binary(value) => TungsteniteMessage::Binary(value),
        AxumMessage::Ping(value) => TungsteniteMessage::Ping(value),
        AxumMessage::Pong(value) => TungsteniteMessage::Pong(value),
        AxumMessage::Close(_) => TungsteniteMessage::Close(None),
    })
}

fn to_lightspeed(message: TungsteniteMessage) -> Option<AxumMessage> {
    match message {
        TungsteniteMessage::Text(value) => Some(AxumMessage::Text(value.to_string().into())),
        TungsteniteMessage::Binary(value) => Some(AxumMessage::Binary(value)),
        TungsteniteMessage::Ping(value) => Some(AxumMessage::Ping(value)),
        TungsteniteMessage::Pong(value) => Some(AxumMessage::Pong(value)),
        TungsteniteMessage::Close(_) => Some(AxumMessage::Close(None)),
        TungsteniteMessage::Frame(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{Router, extract::WebSocketUpgrade, response::IntoResponse as _, routing::get};
    use tokio::{net::TcpListener, sync::Mutex};

    use super::*;

    #[test]
    fn relay_holds_count_per_instance_until_dropped() {
        let relays = Relays::default();
        assert!(!relays.active("a"));
        let first = relays.hold("a");
        let second = relays.hold("a");
        assert!(relays.active("a"));
        assert!(!relays.active("b"));
        drop(first);
        assert!(relays.active("a"));
        drop(second);
        assert!(!relays.active("a"));
        assert!(relays.lock().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn relay_closes_guest_when_lightspeed_disappears_without_a_close_frame() {
        let guest_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("guest listener");
        let guest_address = guest_listener.local_addr().expect("guest address");
        let guest_observer = tokio::spawn(async move {
            let (stream, _) = guest_listener.accept().await.expect("guest accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("guest websocket");
            while let Some(message) = socket.next().await {
                match message {
                    Ok(TungsteniteMessage::Close(_)) => return true,
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
            false
        });
        let (guest, _) = tokio_tungstenite::connect_async(format!("ws://{guest_address}"))
            .await
            .expect("connect guest");
        let guest = Arc::new(Mutex::new(Some(guest)));

        let relay_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let relay_address = relay_listener.local_addr().expect("relay address");
        let app = Router::new().route(
            "/",
            get({
                let guest = guest.clone();
                move |upgrade: WebSocketUpgrade| {
                    let guest = guest.clone();
                    async move {
                        let guest = guest.lock().await.take().expect("one relay connection");
                        upgrade
                            .on_upgrade(move |mut socket| async move {
                                let mut guest = guest;
                                proxy_with_idle(Duration::from_secs(60), &mut socket, &mut guest)
                                    .await;
                            })
                            .into_response()
                    }
                }
            }),
        );
        let relay_server = tokio::spawn(async move {
            axum::serve(relay_listener, app)
                .await
                .expect("relay server")
        });

        let (lightspeed, _) = tokio_tungstenite::connect_async(format!("ws://{relay_address}"))
            .await
            .expect("connect lightspeed");
        drop(lightspeed);

        let saw_close = tokio::time::timeout(Duration::from_secs(2), guest_observer)
            .await
            .expect("guest close timeout")
            .expect("guest observer");
        relay_server.abort();
        assert!(saw_close, "relay dropped guest without a close frame");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn relay_forwards_pings_and_counts_them_as_activity() {
        let guest_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("guest listener");
        let guest_address = guest_listener.local_addr().expect("guest address");
        let guest_observer = tokio::spawn(async move {
            let (stream, _) = guest_listener.accept().await.expect("guest accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("guest websocket");
            let mut pings = 0;
            while let Some(message) = socket.next().await {
                match message {
                    Ok(TungsteniteMessage::Ping(_)) => pings += 1,
                    Ok(TungsteniteMessage::Text(text)) => return Some((pings, text.to_string())),
                    Ok(_) => {}
                    Err(_) => return None,
                }
            }
            None
        });
        let (guest, _) = tokio_tungstenite::connect_async(format!("ws://{guest_address}"))
            .await
            .expect("connect guest");
        let guest = Arc::new(Mutex::new(Some(guest)));
        let relay_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let relay_address = relay_listener.local_addr().expect("relay address");
        let app = Router::new().route(
            "/",
            get(move |upgrade: WebSocketUpgrade| {
                let guest = guest.clone();
                async move {
                    let mut guest = guest.lock().await.take().expect("one relay connection");
                    upgrade
                        .on_upgrade(move |mut socket| async move {
                            proxy_with_idle(Duration::from_millis(300), &mut socket, &mut guest)
                                .await;
                        })
                        .into_response()
                }
            }),
        );
        let relay_server = tokio::spawn(async move {
            axum::serve(relay_listener, app)
                .await
                .expect("relay server")
        });

        let (mut lightspeed, _) = tokio_tungstenite::connect_async(format!("ws://{relay_address}"))
            .await
            .expect("connect lightspeed");
        // Eight pings 100 ms apart keep the 300 ms idle timer from firing
        // across 800 ms with no data frame.
        for _ in 0..8 {
            lightspeed
                .send(TungsteniteMessage::Ping(vec![1].into()))
                .await
                .expect("ping");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        lightspeed
            .send(TungsteniteMessage::Text("after pings".into()))
            .await
            .expect("text");

        let observed = tokio::time::timeout(Duration::from_secs(2), guest_observer)
            .await
            .expect("guest timeout")
            .expect("guest observer");
        relay_server.abort();
        let (pings, text) = observed.expect("the relay closed the guest leg while pings flowed");
        assert_eq!(pings, 8);
        assert_eq!(text, "after pings");
    }
}
