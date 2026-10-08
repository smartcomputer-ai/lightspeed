//! Recover registered content handles from the durable session history.

use std::collections::BTreeMap;

use harness::{
    Attachment, CodeToolEvent, ContentRef, ContextEntryKind, ContextEvent, CoreAgentCodec,
    CoreAgentEvent, CoreAgentIoError, RunEvent, SessionId, ToolEvent,
    media::{MediaDescriptor, media_preview_name},
    storage::{ReadSessionEvents, SessionStore},
};

/// Read recorded descriptors, including entries removed by pruning or compaction.
/// This deliberately pages event metadata rather than replaying the reducer or
/// loading historical tool bodies from CAS. It scans the history on each request;
/// each page is bounded, and only distinct descriptors are retained in memory.
/// Registration resolves short names only, never grants blob access.
pub(super) async fn recorded_content_attachments(
    sessions: &dyn SessionStore,
    session_id: &SessionId,
) -> Result<Vec<Attachment>, CoreAgentIoError> {
    let mut after = None;
    let mut attachments = RecordedAttachments::default();
    loop {
        let page = sessions
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after,
                limit: 1000,
            })
            .await
            .map_err(io_error)?;
        for entry in page.entries {
            if records_content(&entry.event.kind) {
                attachments.record(
                    CoreAgentCodec
                        .decode_event(&entry.event)
                        .map_err(io_error)?,
                );
            }
        }
        if page.complete {
            return Ok(attachments.by_handle.into_values().flatten().collect());
        }
        if page.next_after.is_none() || page.next_after <= after {
            return Err(io_error("session content history page did not advance"));
        }
        after = page.next_after;
    }
}

fn records_content(kind: &str) -> bool {
    matches!(
        kind,
        "lightspeed.core.tool.call_completed"
            | "lightspeed.core.code_tool.call_completed"
            | "lightspeed.core.context.entries_applied"
            | "lightspeed.core.context.entries_replaced"
            | "lightspeed.core.context.state_replaced"
            | "lightspeed.core.context.key_prefix_replaced"
            | "lightspeed.core.run.accepted"
            | "lightspeed.core.run.steering_accepted"
    )
}

#[derive(Default)]
struct RecordedAttachments {
    // Preserve differing descriptors for the same handle so the resolver sees
    // collisions; an arbitrary last record must not silently choose the bytes.
    by_handle: BTreeMap<String, Vec<Attachment>>,
}

impl RecordedAttachments {
    fn insert(&mut self, attachment: Attachment) {
        let variants = self
            .by_handle
            .entry(attachment.handle().to_owned())
            .or_default();
        if !variants.contains(&attachment) {
            variants.push(attachment);
        }
    }

    fn media(&mut self, kind: &ContextEntryKind, content: &ContentRef, preview: Option<&str>) {
        if matches!(kind, ContextEntryKind::Message { .. })
            && let Some(media_type) = content.media_type.as_deref()
            && let Some(media) = MediaDescriptor::new(
                content.content_ref.clone(),
                media_type,
                media_preview_name(preview).as_deref(),
            )
        {
            self.insert(Attachment::Media(media));
        }
    }

    fn inputs(&mut self, inputs: &[harness::ContextEntryInput]) {
        for input in inputs {
            self.media(&input.kind, &input.content, input.preview.as_deref());
        }
    }

    fn record(&mut self, event: CoreAgentEvent) {
        match event {
            CoreAgentEvent::Tool(ToolEvent::CallCompleted { result, .. }) => {
                for attachment in result.attachments {
                    self.insert(attachment);
                }
                self.inputs(&result.model_visible_context_entries);
            }
            CoreAgentEvent::CodeTool(CodeToolEvent::CallCompleted { result, .. }) => {
                for attachment in result.attachments {
                    self.insert(attachment);
                }
            }
            CoreAgentEvent::Context(
                ContextEvent::EntriesApplied { entries, .. }
                | ContextEvent::EntriesReplaced { entries, .. }
                | ContextEvent::StateReplaced { entries, .. }
                | ContextEvent::KeyPrefixReplaced { entries, .. },
            ) => {
                for entry in entries {
                    self.media(&entry.kind, &entry.content, entry.preview.as_deref());
                }
            }
            CoreAgentEvent::Run(RunEvent::Accepted(run)) => self.inputs(run.source.input()),
            CoreAgentEvent::Run(RunEvent::SteeringAccepted { input, .. }) => self.inputs(&input),
            _ => {}
        }
    }
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: format!("read session content registrations: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harness::{
        AcceptedRunEvent, BlobRef, CodeToolOrigin, ContextEntry, ContextEntryId,
        ContextEntrySource, ContextRemovalReason, ContextRewriteReason, FileAttachment, RunId,
        RunSource, SteeringId, StoredEvent, ToolBatchId, ToolCallId, ToolCallResult,
        ToolCallStatus, ToolInvocationResult, TurnId,
        session::UncommittedStoredEvent,
        storage::{
            AppendSessionEvents, BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore,
        },
    };

    use super::*;

    fn media(bytes: &[u8], name: &str) -> MediaDescriptor {
        MediaDescriptor::new(BlobRef::from_bytes(bytes), "image/png", Some(name)).unwrap()
    }

    fn context_entry(media: &MediaDescriptor, id: u64) -> ContextEntry {
        let input = media.context_entry();
        ContextEntry {
            entry_id: ContextEntryId::new(id),
            origin: None,
            key: None,
            kind: input.kind,
            source: ContextEntrySource::ContextEdit,
            content: input.content,
            preview: input.preview,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }
    }

    fn stored(event: CoreAgentEvent) -> UncommittedStoredEvent {
        UncommittedStoredEvent {
            observed_at_ms: 1,
            joins: Default::default(),
            event: CoreAgentCodec.encode_event(&event).unwrap(),
        }
    }

    async fn history(events: Vec<UncommittedStoredEvent>) -> (InMemorySessionStore, SessionId) {
        let sessions = InMemorySessionStore::default();
        let session_id = SessionId::new("content-history");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                display_name: None,
                metadata: Default::default(),
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .unwrap();
        sessions
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events,
            })
            .await
            .unwrap();
        (sessions, session_id)
    }

    fn ordinary_completion(attachment: Attachment) -> CoreAgentEvent {
        CoreAgentEvent::Tool(ToolEvent::CallCompleted {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            result: ToolCallResult {
                call_id: ToolCallId::new("ordinary"),
                status: ToolCallStatus::Succeeded,
                output_ref: None,
                model_visible_context_entries: Vec::new(),
                error_ref: None,
                effects: Vec::new(),
                attachments: vec![attachment],
                duration_ms: None,
                output_bytes: None,
                truncated: false,
            },
        })
    }

    fn code_completion(attachment: Attachment) -> CoreAgentEvent {
        CoreAgentEvent::CodeTool(CodeToolEvent::CallCompleted {
            origin: CodeToolOrigin {
                execution_id: "execution".into(),
                request_id: "request".into(),
            },
            result: ToolInvocationResult {
                call_id: ToolCallId::new("code"),
                status: ToolCallStatus::Succeeded,
                output_ref: None,
                model_visible_context_entries: Vec::new(),
                error_ref: None,
                effects: Vec::new(),
                attachments: vec![attachment],
                duration_ms: None,
                output_bytes: None,
                truncated: false,
            }
            .into(),
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn registrations_survive_context_removal_and_compaction_across_pages() {
        let image = media(b"image", "image.png");
        let ordinary = Attachment::File(FileAttachment::new(
            BlobRef::from_bytes(b"ordinary file"),
            "ordinary.txt".into(),
            Some("text/plain".into()),
        ));
        let code = Attachment::File(FileAttachment::new(
            BlobRef::from_bytes(b"code file"),
            "code.bin".into(),
            None,
        ));
        let mut events = vec![
            stored(CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                base_revision: 0,
                entries: vec![context_entry(&image, 1)],
            })),
            stored(ordinary_completion(ordinary.clone())),
            stored(ordinary_completion(ordinary.clone())),
        ];
        // The second page must still be scanned, and unrelated events are not
        // decoded as core events or followed into arbitrary blob payloads.
        events.extend((0..1000).map(|_| UncommittedStoredEvent {
            observed_at_ms: 1,
            joins: Default::default(),
            event: StoredEvent::new("lightspeed.test.unrelated", 99, serde_json::Value::Null),
        }));
        events.extend([
            stored(code_completion(code.clone())),
            stored(CoreAgentEvent::Context(ContextEvent::EntriesRemoved {
                base_revision: 1,
                entry_ids: vec![ContextEntryId::new(1)],
                reason: ContextRemovalReason::Pruned,
            })),
            stored(CoreAgentEvent::Context(ContextEvent::StateReplaced {
                base_revision: 2,
                entries: Vec::new(),
                reason: ContextRewriteReason::ProviderCompacted,
            })),
        ]);
        let (sessions, session_id) = history(events).await;

        let attachments = recorded_content_attachments(&sessions, &session_id)
            .await
            .unwrap();

        assert_eq!(attachments.len(), 3);
        assert!(attachments.contains(&Attachment::Media(image.clone())));
        assert!(attachments.contains(&ordinary));
        assert!(attachments.contains(&code));

        let blobs = Arc::new(InMemoryBlobStore::default());
        blobs.put_bytes(b"image".to_vec()).await.unwrap();
        let resolver = tools::content::ContentResolver::new(blobs).with_attachments(attachments);
        let resolved = resolver
            .resolve(&tools::content::ContentReference::Reference(image.handle))
            .await
            .expect("historical handle remains usable after compaction");
        assert_eq!(resolved.content_ref, image.content_ref);
        let unknown = resolver
            .resolve(&tools::content::ContentReference::Reference(
                "media:000000000000".into(),
            ))
            .await
            .expect_err("history does not register unknown aliases");
        assert!(matches!(
            unknown,
            tools::ToolError::Content(tools::content::ContentError::UnknownHandle { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn input_steering_and_context_replacements_register_media() {
        let accepted = media(b"accepted", "input.png");
        let steering = media(b"steering", "steering.png");
        let replaced = media(b"replaced", "replacement.png");
        let keyed = media(b"keyed", "keyed.png");
        let state = media(b"state", "state.png");
        let (sessions, session_id) = history(vec![
            stored(CoreAgentEvent::Run(RunEvent::Accepted(AcceptedRunEvent {
                run_id: RunId::new(1),
                submission_id: None,
                source: RunSource::Input {
                    input: vec![accepted.context_entry()],
                },
                run_config: Default::default(),
                config_revision: 0,
                notify_on_terminal: Vec::new(),
                requested_by: None,
            }))),
            stored(CoreAgentEvent::Run(RunEvent::SteeringAccepted {
                run_id: RunId::new(1),
                steering_id: SteeringId::new(1),
                input: vec![steering.context_entry()],
                requested_by: None,
            })),
            stored(CoreAgentEvent::Context(ContextEvent::EntriesReplaced {
                base_revision: 0,
                entries: vec![context_entry(&replaced, 1)],
            })),
            stored(CoreAgentEvent::Context(ContextEvent::KeyPrefixReplaced {
                base_revision: 1,
                key_prefix: harness::ContextEntryKey::new("test"),
                entries: vec![context_entry(&keyed, 2)],
            })),
            stored(CoreAgentEvent::Context(ContextEvent::StateReplaced {
                base_revision: 2,
                entries: vec![context_entry(&state, 3)],
                reason: ContextRewriteReason::PolicyChanged,
            })),
        ])
        .await;

        let attachments = recorded_content_attachments(&sessions, &session_id)
            .await
            .unwrap();

        assert_eq!(attachments.len(), 5);
        for descriptor in [accepted, steering, replaced, keyed, state] {
            assert!(attachments.contains(&Attachment::Media(descriptor)));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn conflicting_registered_handles_are_preserved_for_resolution() {
        let first = Attachment::File(FileAttachment::new(
            BlobRef::parse(format!("sha256:{}{}", "a".repeat(24), "0".repeat(40))).unwrap(),
            "first.bin".into(),
            None,
        ));
        let second = Attachment::File(FileAttachment::new(
            BlobRef::parse(format!("sha256:{}{}", "a".repeat(24), "1".repeat(40))).unwrap(),
            "second.bin".into(),
            None,
        ));
        assert_eq!(first.handle(), second.handle());
        let (sessions, session_id) = history(vec![
            stored(ordinary_completion(first.clone())),
            stored(code_completion(second.clone())),
        ])
        .await;

        let attachments = recorded_content_attachments(&sessions, &session_id)
            .await
            .unwrap();

        assert_eq!(attachments.len(), 2);
        assert!(attachments.contains(&first));
        assert!(attachments.contains(&second));
        let resolver = tools::content::ContentResolver::new(Arc::new(InMemoryBlobStore::default()))
            .with_attachments(attachments);
        let error = resolver
            .resolve(&tools::content::ContentReference::Reference(
                first.handle().into(),
            ))
            .await
            .expect_err("colliding aliases cannot choose a target");
        assert!(matches!(
            error,
            tools::ToolError::Content(tools::content::ContentError::AmbiguousHandle { .. })
        ));
    }
}
