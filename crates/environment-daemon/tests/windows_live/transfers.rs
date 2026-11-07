use super::support::*;
use environment_client::DataClient;
use environment_protocol::{
    data::{
        fs::{TreeContent, TreeFilter},
        transfer::*,
    },
    error::{BusyReason, ErrorData, ProtocolErrorKind},
    ids::TransferOperationId,
    paths::{ContentDigest, TreePath},
};
use sha2::{Digest, Sha256};

fn operation(id: &str) -> TransferOperationParams {
    TransferOperationParams {
        operation_id: TransferOperationId::new(id).unwrap(),
    }
}
fn limits() -> TransferLimits {
    TransferLimits {
        max_entries: 100,
        max_depth: 16,
        max_file_bytes: 4 * 1024 * 1024,
        max_total_bytes: 4 * 1024 * 1024,
        max_manifest_bytes: 65536,
        max_duration_ms: 60_000,
    }
}
fn publish(daemon: &Daemon, id: &str, destination: &str) -> BeginTransferParams {
    BeginTransferParams {
        operation_id: operation(id).operation_id,
        selection: TransferSelection::Materialize {
            destination: daemon.path(destination),
            on_existing: TransferOnExisting::Replace,
        },
        limits: limits(),
    }
}
fn capture(daemon: &Daemon, id: &str, source: &str) -> BeginTransferParams {
    BeginTransferParams {
        operation_id: operation(id).operation_id,
        selection: TransferSelection::Capture {
            source: daemon.path(source),
            filter: TreeFilter::default(),
            symlinks: Default::default(),
            special_files: Default::default(),
        },
        limits: limits(),
    }
}
fn digest(bytes: &[u8]) -> ContentDigest {
    ContentDigest::from_sha256(Sha256::digest(bytes).into())
}
async fn advance(client: &DataClient, id: &str, phase: TransferPhase) -> TransferStatus {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let status = client.transfer_advance(&operation(id)).await.unwrap();
            if status.phase == phase {
                return status;
            }
            assert!(!status.phase.is_finished(), "{status:?}");
        }
    })
    .await
    .expect("transfer did not advance")
}
async fn append(client: &DataClient, id: &str, bytes: &[u8]) {
    client
        .transfer_append(&AppendTransferInventoryParams {
            operation_id: operation(id).operation_id,
            index: 0,
            entries: vec![InventoryEntry {
                path: TreePath::root(),
                content: TreeContent::File {
                    size_bytes: bytes.len() as u64,
                    executable: false,
                    digest: Some(digest(bytes)),
                },
            }],
            last: true,
        })
        .await
        .unwrap();
    advance(client, id, TransferPhase::Content).await;
}
fn chunk(id: &str, content: &[u8], offset: usize, end: usize) -> WriteTransferContentParams {
    WriteTransferContentParams {
        operation_id: operation(id).operation_id,
        digest: digest(content),
        offset: offset as u64,
        data: content[offset..end].into(),
    }
}
pub async fn read_only(daemon: &Daemon) {
    daemon
        .client
        .transfer_begin(&capture(daemon, "capture", "existing"))
        .await
        .unwrap();
    advance(&daemon.client, "capture", TransferPhase::Inventory).await;
    assert_eq!(
        daemon
            .client
            .transfer_commit(&operation("capture"))
            .await
            .unwrap()
            .phase,
        TransferPhase::Complete
    );
    let error = daemon
        .client
        .transfer_begin(&publish(daemon, "write", "new"))
        .await
        .unwrap_err();
    assert_eq!(
        error.protocol_kind(),
        Some(ProtocolErrorKind::CapabilityUnavailable)
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "crashes real envd during inventory, partial upload and staged publication"]
async fn interrupted_transfers_recover_after_actual_process_death() {
    for phase in [
        TransferPhase::Inventory,
        TransferPhase::Content,
        TransferPhase::Ready,
    ] {
        let mut daemon = Daemon::start(false).await;
        let content = b"content withheld until commit";
        daemon
            .client
            .transfer_begin(&publish(&daemon, "interrupted", "not published 空"))
            .await
            .unwrap();
        if phase != TransferPhase::Inventory {
            append(&daemon.client, "interrupted", content).await;
            let end = if phase == TransferPhase::Ready {
                content.len()
            } else {
                7
            };
            daemon
                .client
                .transfer_write(&chunk("interrupted", content, 0, end))
                .await
                .unwrap();
        }
        if phase == TransferPhase::Ready {
            advance(&daemon.client, "interrupted", TransferPhase::Ready).await;
            assert!(
                std::fs::read_dir(&daemon.root)
                    .unwrap()
                    .flatten()
                    .any(|entry| entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".lightspeed-transfer-"))
            );
        }
        daemon.restart().await;
        let status = daemon
            .client
            .transfer_get_status(&operation("interrupted"))
            .await
            .unwrap();
        assert_eq!(
            status.phase,
            TransferPhase::Failed,
            "crash phase: {phase:?}"
        );
        assert_eq!(status.error.unwrap().kind(), ProtocolErrorKind::Aborted);
        assert!(!daemon.root.join("not published 空").exists());
        assert!(
            !std::fs::read_dir(&daemon.root)
                .unwrap()
                .flatten()
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".lightspeed-transfer-"))
        );
        assert!(
            !std::fs::read_dir(daemon.state.join("transfers"))
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".spool-"))
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "uses independent WebSockets, concurrent publishers and restart-persisted receipts"]
async fn competing_publishers_reconnect_and_replay_receipts_after_restart() {
    let mut daemon = Daemon::start(false).await;
    let clients = futures_util::future::join_all((0..8).map(|_| connect(&daemon.endpoint))).await;
    let requests: Vec<_> = (0..8)
        .map(|index| {
            publish(
                &daemon,
                &format!("writer-{index}"),
                if index % 2 == 0 {
                    "Published.bin"
                } else {
                    "published.bin"
                },
            )
        })
        .collect();
    let replies = futures_util::future::join_all(
        clients
            .iter()
            .zip(&requests)
            .map(|(client, request)| client.transfer_begin(request)),
    )
    .await;
    let mut winners = Vec::new();
    for (index, reply) in replies.into_iter().enumerate() {
        match reply {
            Ok(_) => winners.push(index),
            Err(error) => assert!(
                matches!(
                    error.protocol_error().map(|e| &e.data),
                    Some(ErrorData::Busy {
                        reason: Some(BusyReason::TransferCapacity),
                        ..
                    })
                ),
                "unexpected competing publisher error: {error}"
            ),
        }
    }
    assert_eq!(
        winners.len(),
        1,
        "overlapping publishers must be mutually exclusive"
    );
    let winner = winners[0];
    let id = requests[winner].operation_id.as_str();
    let content: Vec<u8> = (0..1024 * 1024 + 17).map(|i| (i % 251) as u8).collect();
    append(&clients[winner], id, &content).await;
    let first = chunk(id, &content, 0, 65536);
    clients[winner].transfer_write(&first).await.unwrap();
    for client in &clients {
        client.close().await.unwrap();
    }
    let reconnected = connect(&daemon.endpoint).await;
    assert_eq!(
        reconnected
            .transfer_write(&first)
            .await
            .unwrap()
            .next_offset,
        65536
    );
    let mut offset = 65536;
    while offset < content.len() {
        let end = (offset + 65536).min(content.len());
        assert_eq!(
            reconnected
                .transfer_write(&chunk(id, &content, offset, end))
                .await
                .unwrap()
                .next_offset,
            end as u64
        );
        offset = end;
    }
    advance(&reconnected, id, TransferPhase::Ready).await;
    let receipt = reconnected.transfer_commit(&operation(id)).await.unwrap();
    assert_eq!(
        std::fs::read(daemon.root.join("published.bin")).unwrap(),
        content
    );
    let captured = capture(&daemon, "capture-published", "published.bin");
    reconnected.transfer_begin(&captured).await.unwrap();
    advance(&reconnected, "capture-published", TransferPhase::Inventory).await;
    let inventory_request = ReadTransferInventoryParams {
        operation_id: captured.operation_id.clone(),
        index: 0,
        limit: 10,
    };
    let inventory = reconnected
        .transfer_read_inventory(&inventory_request)
        .await
        .unwrap();
    let captured_receipt = reconnected
        .transfer_commit(&operation("capture-published"))
        .await
        .unwrap();
    std::fs::write(
        daemon.root.join("published.bin"),
        b"local edit after commit",
    )
    .unwrap();
    daemon.restart().await;
    assert_eq!(
        daemon
            .client
            .transfer_begin(&requests[winner])
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        daemon.client.transfer_begin(&captured).await.unwrap(),
        captured_receipt
    );
    assert_eq!(
        daemon
            .client
            .transfer_read_inventory(&inventory_request)
            .await
            .unwrap(),
        inventory
    );
    assert_eq!(
        std::fs::read(daemon.root.join("published.bin")).unwrap(),
        b"local edit after commit"
    );
}
