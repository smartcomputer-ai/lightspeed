use agent_core::{
    ContextEvent, ContextItem, ContextItemSource, CoreAgentEntry, CoreAgentEventKind, RunId,
};

pub(crate) struct CoreAgentProjection<'a> {
    entries: &'a [CoreAgentEntry],
}

impl<'a> CoreAgentProjection<'a> {
    pub(crate) fn new(entries: &'a [CoreAgentEntry]) -> Self {
        Self { entries }
    }

    pub(crate) fn entries(&self) -> &'a [CoreAgentEntry] {
        self.entries
    }

    pub(crate) fn context_items_for_run(&self, run_id: RunId) -> Vec<&'a ContextItem> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let CoreAgentEventKind::Context(ContextEvent::ItemsRecorded { items }) =
                    &entry.event.kind
                else {
                    return None;
                };
                Some(
                    items
                        .iter()
                        .filter(move |item| context_item_run_id(item) == Some(run_id)),
                )
            })
            .flatten()
            .collect()
    }
}

pub(crate) fn context_item_run_id(item: &ContextItem) -> Option<RunId> {
    match &item.source {
        ContextItemSource::RunInput { run_id }
        | ContextItemSource::Steering { run_id }
        | ContextItemSource::AssistantOutput { run_id, .. }
        | ContextItemSource::ToolCall { run_id, .. }
        | ContextItemSource::ToolResult { run_id, .. }
        | ContextItemSource::Reasoning { run_id, .. } => Some(*run_id),
        ContextItemSource::Compaction { run_id, .. } => *run_id,
        ContextItemSource::Instructions | ContextItemSource::Runtime { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use agent_core::{
        BlobRef, ContextItemId, ContextItemKind, ContextMessageRole, CoreAgentEvent,
        CoreAgentJoins, EventSeq, SessionPosition,
    };

    use super::*;

    #[test]
    fn context_items_for_run_reads_committed_item_events() {
        let first = context_item(
            1,
            ContextItemSource::RunInput {
                run_id: RunId::new(1),
            },
        );
        let second = context_item(
            2,
            ContextItemSource::RunInput {
                run_id: RunId::new(2),
            },
        );
        let entries = vec![entry(1, vec![first]), entry(2, vec![second])];

        let projected = CoreAgentProjection::new(&entries).context_items_for_run(RunId::new(1));

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].item_id, ContextItemId::new(1));
    }

    fn entry(seq: u64, items: Vec<ContextItem>) -> CoreAgentEntry {
        CoreAgentEntry {
            position: SessionPosition {
                seq: EventSeq::new(seq),
            },
            observed_at_ms: seq,
            joins: CoreAgentJoins::default(),
            event: CoreAgentEvent {
                kind: CoreAgentEventKind::Context(ContextEvent::ItemsRecorded { items }),
            },
        }
    }

    fn context_item(id: u64, source: ContextItemSource) -> ContextItem {
        ContextItem {
            item_id: ContextItemId::new(id),
            kind: ContextItemKind::Message {
                role: ContextMessageRole::User,
            },
            source,
            native_item_ref: BlobRef::default(),
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }
}
