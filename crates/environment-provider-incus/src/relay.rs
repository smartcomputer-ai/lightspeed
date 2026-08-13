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
    loop {
        let event = tokio::time::timeout(idle, async {
            tokio::select! {
                message = lightspeed.recv() => Event::Lightspeed(message),
                message = guest.next() => Event::Guest(message),
            }
        })
        .await;
        let Ok(event) = event else {
            let _ = lightspeed.send(AxumMessage::Close(None)).await;
            let _ = guest.close(None).await;
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
}

enum Event {
    Lightspeed(Option<Result<AxumMessage, axum::Error>>),
    Guest(Option<Result<TungsteniteMessage, tokio_tungstenite::tungstenite::Error>>),
}

fn to_guest(message: AxumMessage) -> Option<TungsteniteMessage> {
    Some(match message {
        AxumMessage::Text(value) => TungsteniteMessage::Text(value.to_string().into()),
        AxumMessage::Binary(value) => TungsteniteMessage::Binary(value.to_vec().into()),
        AxumMessage::Ping(value) => TungsteniteMessage::Ping(value.to_vec().into()),
        AxumMessage::Pong(value) => TungsteniteMessage::Pong(value.to_vec().into()),
        AxumMessage::Close(_) => TungsteniteMessage::Close(None),
    })
}

fn to_lightspeed(message: TungsteniteMessage) -> Option<AxumMessage> {
    match message {
        TungsteniteMessage::Text(value) => Some(AxumMessage::Text(value.to_string())),
        TungsteniteMessage::Binary(value) => Some(AxumMessage::Binary(value.to_vec())),
        TungsteniteMessage::Ping(value) => Some(AxumMessage::Ping(value.to_vec())),
        TungsteniteMessage::Pong(value) => Some(AxumMessage::Pong(value.to_vec())),
        TungsteniteMessage::Close(_) => Some(AxumMessage::Close(None)),
        TungsteniteMessage::Frame(_) => None,
    }
}
