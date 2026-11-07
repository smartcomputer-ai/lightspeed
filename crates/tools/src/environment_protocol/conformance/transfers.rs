use super::*;
use environment_protocol::{
    data::{fs::TreeContent, transfer::*},
    ids::TransferOperationId,
};

impl Context<'_> {
    async fn transfer_phase(
        &self,
        operation: &TransferOperationParams,
        phase: TransferPhase,
    ) -> Result<TransferStatus, String> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let status = call(self.client.transfer_advance(operation)).await?;
            if status.phase == phase {
                return Ok(status);
            }
            ensure(
                !status.phase.is_finished(),
                format!("transfer ended before {phase:?}: {status:?}"),
            )?;
            ensure(
                Instant::now() < deadline,
                "transfer did not reach its next phase",
            )?;
        }
    }

    pub(super) async fn transfers(&self) -> Check {
        let ceilings = &self.initialized.limits.filesystem.transfer;
        let limits = TransferLimits {
            max_entries: ceilings.max_entries.min(4),
            max_depth: ceilings.max_depth.min(4),
            max_file_bytes: ceilings.max_file_bytes.min(1024),
            max_total_bytes: ceilings.max_total_bytes.min(1024),
            max_manifest_bytes: ceilings.max_manifest_bytes.min(4096),
            max_duration_ms: ceilings
                .max_duration_ms
                .min(self.timeout.as_millis() as u64),
        };
        let operation = || TransferOperationParams {
            operation_id: TransferOperationId::new(format!(
                "conformance-{}",
                uuid::Uuid::new_v4().simple()
            ))
            .expect("generated id"),
        };
        let bytes: &[u8] = b"\0binary transfer\xff";
        self.write("capture ü.bin", bytes).await?;
        let capture = operation();
        let begin = BeginTransferParams {
            operation_id: capture.operation_id.clone(),
            selection: TransferSelection::Capture {
                source: self.path("capture ü.bin")?,
                filter: Default::default(),
                symlinks: Default::default(),
                special_files: Default::default(),
            },
            limits,
        };
        call(self.client.transfer_begin(&begin)).await?;
        self.transfer_phase(&capture, TransferPhase::Inventory)
            .await?;
        let page_params = ReadTransferInventoryParams {
            operation_id: capture.operation_id.clone(),
            index: 0,
            limit: 1,
        };
        let inventory = call(self.client.transfer_read_inventory(&page_params)).await?;
        ensure(
            inventory.entries.len() == 1 && inventory.next_index.is_none(),
            "single-file capture returned the wrong inventory",
        )?;
        let entry = inventory.entries[0].clone();
        let TreeContent::File {
            digest: Some(digest),
            size_bytes,
            ..
        } = entry.content
        else {
            return Err("capture omitted its digest".into());
        };
        ensure(
            size_bytes == bytes.len() as u64,
            "capture reported the wrong size",
        )?;
        let mut copied = Vec::new();
        while copied.len() < bytes.len() {
            let chunk = call(self.client.transfer_read(&ReadTransferContentParams {
                operation_id: capture.operation_id.clone(),
                digest,
                offset: copied.len() as u64,
                max_bytes: ceilings.max_content_chunk_bytes.min(4),
            }))
            .await?;
            ensure(
                !chunk.data.is_empty(),
                "capture returned no bytes before EOF",
            )?;
            copied.extend_from_slice(chunk.data.as_slice());
        }
        ensure(copied == bytes, "capture changed binary bytes")?;
        let receipt = call(self.client.transfer_commit(&capture)).await?;
        ensure(
            call(self.client.transfer_begin(&begin)).await? == receipt,
            "capture begin did not replay its receipt",
        )?;
        ensure(
            call(self.client.transfer_read_inventory(&page_params)).await? == inventory,
            "receipt lost its inventory",
        )?;

        for stale in [true, false] {
            let before = self.write("publish ü.bin", b"old").await?;
            let publish = operation();
            call(self.client.transfer_begin(&BeginTransferParams {
                operation_id: publish.operation_id.clone(),
                selection: TransferSelection::WriteFile {
                    destination: self.path("publish ü.bin")?,
                    create: WriteCondition::IfPresent,
                    if_match: Some(before.version),
                    executable: false,
                },
                limits,
            }))
            .await?;
            call(self.client.transfer_append(&AppendTransferInventoryParams {
                operation_id: publish.operation_id.clone(),
                index: 0,
                entries: vec![entry.clone()],
                last: true,
            }))
            .await?;
            self.transfer_phase(&publish, TransferPhase::Content)
                .await?;
            let missing = call(
                self.client
                    .transfer_list_missing(&ListTransferMissingParams {
                        operation_id: publish.operation_id.clone(),
                        limit: 1,
                    }),
            )
            .await?;
            ensure(
                missing.digests == [digest],
                "publication did not request its missing content",
            )?;
            let mut offset = 0;
            while offset < bytes.len() {
                let end =
                    (offset + ceilings.max_content_chunk_bytes.min(4) as usize).min(bytes.len());
                ensure(end > offset, "content chunk ceiling is zero")?;
                call(self.client.transfer_write(&WriteTransferContentParams {
                    operation_id: publish.operation_id.clone(),
                    digest,
                    offset: offset as u64,
                    data: bytes[offset..end].into(),
                }))
                .await?;
                offset = end;
            }
            self.transfer_phase(&publish, TransferPhase::Ready).await?;
            if stale {
                self.write("publish ü.bin", b"concurrent edit").await?;
                expect_precondition(
                    self.client.transfer_commit(&publish).await,
                    PreconditionReason::IfMatchFailed,
                )?;
            } else {
                let receipt = call(self.client.transfer_commit(&publish)).await?;
                ensure(
                    receipt.phase == TransferPhase::Complete,
                    "publication did not finish",
                )?;
                ensure(
                    call(self.client.transfer_commit(&publish)).await? == receipt,
                    "commit retry changed its receipt",
                )?;
            }
            let read = call(self.client.read_file(&ReadFileParams {
                path: self.path("publish ü.bin")?,
                offset: 0,
                max_bytes: 64,
            }))
            .await?;
            ensure(
                read.data.as_slice() == if stale { b"concurrent edit" } else { bytes },
                "publication overwrote a stale version or changed its bytes",
            )?;
        }
        Ok(())
    }
}
