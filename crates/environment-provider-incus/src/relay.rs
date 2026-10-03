//! On-demand provider data connections.
//!
//! Lightspeed opens one provider WebSocket for a specific current target. The
//! provider validates ownership, dials the target's private envd, and proxies
//! the connection until it closes or becomes idle. There is no provider-initiated
//! connection and no provider route registry in Lightspeed.

use std::time::Duration;

use anyhow::Context as _;
use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message as TungsteniteMessage,
};

use crate::{Config, incus::OwnedTarget};

pub type GuestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn dial_guest(config: &Config, target: &OwnedTarget) -> anyhow::Result<GuestSocket> {
    let address = target
        .ipv4_address
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("target has no private IPv4 address"))?;
    let endpoint = format!("ws://{address}:{}/", config.envd_port);
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(config.dial_timeout_seconds),
        connect_async(endpoint),
    )
    .await
    .context("envd dial timeout")??;
    Ok(socket)
}

pub async fn probe_guest(config: &Config, target: &OwnedTarget) -> bool {
    match dial_guest(config, target).await {
        Ok(mut socket) => {
            let _ = socket.close(None).await;
            true
        }
        Err(_) => false,
    }
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

    #[tokio::test]
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
        let (guest, _) = connect_async(format!("ws://{guest_address}"))
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

        let (lightspeed, _) = connect_async(format!("ws://{relay_address}"))
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
}
