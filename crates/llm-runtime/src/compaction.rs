//! Bounded standalone compaction shared by native and summary adapters.
use crate::{LlmAdapterError, LlmAdapterResult, LlmCompactionAdapter};
use engine::{
    ContextCompactionRequest, ContextCompactionResult, ContextCompactionStatus, ContextEntry,
    ContextEntryInput, ContextEntryKind, ContextEntrySource,
};
use std::collections::BTreeSet;

pub(crate) fn native_compaction_unavailable(error: &llm_clients::LlmApiError) -> bool {
    matches!(error, llm_clients::LlmApiError::Unsupported(_))
        || matches!(error, llm_clients::LlmApiError::HttpStatus(error) if matches!(error.status, 404 | 405 | 501))
}

const MAX_CALLS: usize = 32;
const MAX_INPUT_TOKENS: u64 = 2_000_000;

fn limit_error(message: &str) -> LlmAdapterError {
    LlmAdapterError::InvalidProviderRequest {
        message: message.into(),
    }
}

fn is_context_limit(error: &LlmAdapterError) -> bool {
    match error {
        LlmAdapterError::ContextLimit { .. } => true,
        LlmAdapterError::Provider { source } => source
            .request_rejection()
            .is_some_and(|r| r.kind == llm_clients::ProviderFailureKind::ContextLength),
        _ => false,
    }
}

/// Cuts preserve complete assistant/tool exchanges. Do not split parallel tool calls.
fn safe_cuts(entries: &[ContextEntry]) -> Vec<usize> {
    let mut cuts = vec![0];
    let mut open = BTreeSet::new();
    let mut previous = None;
    for (index, entry) in entries.iter().enumerate() {
        let turn = match entry.source {
            ContextEntrySource::AssistantOutput { run_id, turn_id }
            | ContextEntrySource::Reasoning { run_id, turn_id }
            | ContextEntrySource::Tool {
                run_id, turn_id, ..
            } => Some((run_id, turn_id)),
            _ => None,
        };
        let same_native_window = index > 0
            && matches!((&entries[index - 1].source, &entry.source),
            (ContextEntrySource::Runtime { label: previous }, ContextEntrySource::Runtime { label })
                if previous == engine::STANDALONE_COMPACTION_SOURCE && label == previous);
        if index > 0
            && !same_native_window
            && (turn != previous || turn.is_none())
            && open.is_empty()
        {
            cuts.push(index);
        }
        match &entry.kind {
            ContextEntryKind::ToolCall { call_id, .. } => {
                open.insert(call_id.clone());
            }
            ContextEntryKind::ToolResult { call_id, .. } => {
                open.remove(call_id);
            }
            _ => {}
        }
        previous = turn;
    }
    if open.is_empty() {
        cuts.push(entries.len());
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

pub(crate) async fn compact(
    adapter: &dyn LlmCompactionAdapter,
    request: ContextCompactionRequest,
) -> LlmAdapterResult<ContextCompactionResult> {
    // Old callers send their complete window; preserve that contract.
    if request.request.covered_entry_ids.is_empty() {
        return adapter.compact_context(request).await;
    }
    let covered: BTreeSet<_> = request.request.covered_entry_ids.iter().copied().collect();
    let canonical: Vec<_> = request
        .request
        .context
        .entries
        .iter()
        .filter(|e| !covered.contains(&e.entry_id))
        .cloned()
        .collect();
    let prefix: Vec<_> = request
        .request
        .context
        .entries
        .iter()
        .filter(|e| covered.contains(&e.entry_id))
        .cloned()
        .collect();
    let cuts = safe_cuts(&prefix);
    if cuts.last().copied() != Some(prefix.len()) || prefix.is_empty() {
        return Err(limit_error(
            "compaction prefix contains an unanswered tool call or no history",
        ));
    }
    let mut start = 0;
    let mut window: Vec<ContextEntryInput> = Vec::new();
    let mut calls = 0;
    let mut spent = 0u64;
    let mut usage: Option<engine::LlmUsage> = None;
    while start < prefix.len() {
        let mut end = prefix.len();
        loop {
            if calls >= MAX_CALLS {
                return Err(limit_error("compaction exhausted its 32-call budget"));
            }
            let mut part = request.clone();
            part.request.context.entries = canonical
                .iter()
                .filter(|entry| matches!(entry.kind, ContextEntryKind::Instructions))
                .cloned()
                .collect();
            for (index, entry) in window.iter().cloned().enumerate() {
                part.request.context.entries.push(synthetic_entry(
                    entry,
                    u64::MAX - window.len() as u64 + index as u64,
                ));
            }
            part.request
                .context
                .entries
                .extend_from_slice(&prefix[start..end]);
            part.request.context.entries.extend(
                canonical
                    .iter()
                    .filter(|entry| !matches!(entry.kind, ContextEntryKind::Instructions))
                    .cloned(),
            );
            let size = estimate(adapter, &part.request.context.entries).await?;
            if let Some(budget) = part.request.input_limit_tokens
                && size > u64::from(budget).saturating_mul(4) / 5
            {
                end = smaller_end(&cuts, start, end)?;
                continue;
            }
            spent = spent.saturating_add(size);
            if spent > MAX_INPUT_TOKENS {
                return Err(limit_error("compaction exhausted its input token budget"));
            }
            calls += 1;
            match adapter.compact_context(part).await {
                Err(error) if is_context_limit(&error) => {
                    end = smaller_end(&cuts, start, end)?;
                }
                Err(error) => return Err(error),
                Ok(result) => {
                    if result.status != ContextCompactionStatus::Succeeded
                        || result.context_entries.is_empty()
                    {
                        return Err(limit_error(
                            "compaction produced no complete usable summary",
                        ));
                    }
                    // Text summaries must reduce large inputs; opaque native bytes are not token counts.
                    if result
                        .context_entries
                        .iter()
                        .all(|e| matches!(e.kind, ContextEntryKind::Message { .. }))
                        && size > 4096
                    {
                        let entries: Vec<_> = result
                            .context_entries
                            .iter()
                            .cloned()
                            .enumerate()
                            .map(|(i, e)| synthetic_entry(e, i as u64 + 1))
                            .collect();
                        if estimate(adapter, &entries).await? >= size {
                            return Err(limit_error("compaction did not reduce its input"));
                        }
                    }
                    calls += result.calls.saturating_sub(1) as usize;
                    if calls > MAX_CALLS {
                        return Err(limit_error("compaction exhausted its call budget"));
                    }
                    if let Some(value) = result.usage {
                        if let Some(input) = value.input_tokens {
                            spent = spent.saturating_sub(size).saturating_add(u64::from(input));
                        }
                        if spent > MAX_INPUT_TOKENS {
                            return Err(limit_error("compaction exhausted its input token budget"));
                        }
                        add_usage(&mut usage, value);
                    }
                    window = result.context_entries;
                    start = end;
                    break;
                }
            }
        }
    }
    Ok(ContextCompactionResult {
        usage,
        calls: calls as u32,
        session_id: request.session_id,
        context_revision: request.request.context.context_revision,
        status: ContextCompactionStatus::Succeeded,
        failure_ref: None,
        context_entries: window,
    })
}

fn smaller_end(cuts: &[usize], start: usize, end: usize) -> LlmAdapterResult<usize> {
    let candidates: Vec<_> = cuts
        .iter()
        .copied()
        .filter(|cut| *cut > start && *cut < end)
        .collect();
    candidates
        .get(candidates.len() / 2)
        .copied()
        .ok_or_else(|| {
            limit_error("the smallest complete compaction chunk exceeds the context window")
        })
}

async fn estimate(
    adapter: &dyn LlmCompactionAdapter,
    entries: &[ContextEntry],
) -> LlmAdapterResult<u64> {
    let mut tokens = 0u64;
    for entry in entries {
        tokens = tokens.saturating_add(if let Some(estimate) = &entry.token_estimate {
            u64::from(estimate.tokens)
        } else if let Some(blobs) = adapter.blobs() {
            let bytes = blobs.read_bytes(&entry.content.content_ref).await?;
            (bytes.len() as u64).div_ceil(2)
        } else {
            256
        });
    }
    Ok(tokens)
}

fn synthetic_entry(input: ContextEntryInput, id: u64) -> ContextEntry {
    ContextEntry {
        entry_id: engine::ContextEntryId::new(id),
        key: None,
        source: ContextEntrySource::Runtime {
            label: "rolling_compaction".into(),
        },
        kind: input.kind,
        content: input.content,
        preview: input.preview,
        origin: input.origin,
        provenance_ref: input.provenance_ref,
        token_estimate: input.token_estimate,
        supersedes: None,
    }
}

fn add_usage(total: &mut Option<engine::LlmUsage>, value: engine::LlmUsage) {
    if let Some(total) = total {
        for (target, value) in [
            (&mut total.input_tokens, value.input_tokens),
            (&mut total.output_tokens, value.output_tokens),
            (&mut total.total_tokens, value.total_tokens),
            (&mut total.reasoning_tokens, value.reasoning_tokens),
            (&mut total.cached_input_tokens, value.cached_input_tokens),
            (
                &mut total.cache_write_input_tokens,
                value.cache_write_input_tokens,
            ),
            (
                &mut total.cache_miss_input_tokens,
                value.cache_miss_input_tokens,
            ),
        ] {
            if let Some(value) = value {
                *target = Some(target.unwrap_or(0).saturating_add(value));
            }
        }
    } else {
        *total = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use engine::{
        BlobRef, ContentRef, ContextCompactionTask, ContextEntryId, ContextMessageRole,
        ContextSnapshot, ModelSelection, ProviderApiKind, SessionId, TokenEstimate,
        TokenEstimateQuality, ToolCallId, ToolName,
    };
    use std::sync::Mutex;

    struct BoundedAdapter {
        seen: Mutex<Vec<Vec<ContextEntry>>>,
        reject_after_success: bool,
    }
    #[async_trait]
    impl LlmCompactionAdapter for BoundedAdapter {
        async fn compact_context(
            &self,
            request: ContextCompactionRequest,
        ) -> LlmAdapterResult<ContextCompactionResult> {
            let entries = request.request.context.entries;
            let count = entries
                .iter()
                .filter(|entry| entry.entry_id.as_u64() < 100)
                .count();
            let mut seen = self.seen.lock().unwrap();
            let already_succeeded = seen.iter().any(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.entry_id.as_u64() < 100)
                    .count()
                    <= 2
            });
            seen.push(entries);
            if count > 2 {
                return Err(LlmAdapterError::ContextLimit {
                    message: "typed overflow".into(),
                });
            }
            if already_succeeded && self.reject_after_success {
                return Err(limit_error("ordinary invalid request"));
            }
            Ok(ContextCompactionResult {
                session_id: request.session_id,
                context_revision: request.request.context.context_revision,
                status: ContextCompactionStatus::Succeeded,
                failure_ref: None,
                calls: 1,
                usage: Some(engine::LlmUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    total_tokens: Some(12),
                    reasoning_tokens: None,
                    cached_input_tokens: None,
                    cache_write_input_tokens: None,
                    cache_miss_input_tokens: None,
                }),
                context_entries: vec![ContextEntryInput {
                    kind: ContextEntryKind::Message {
                        role: ContextMessageRole::User,
                    },
                    content: ContentRef::text(BlobRef::from_bytes(b"summary")),
                    preview: None,
                    origin: None,
                    provenance_ref: None,
                    token_estimate: Some(TokenEstimate {
                        tokens: 1,
                        quality: TokenEstimateQuality::Estimated,
                    }),
                }],
            })
        }
    }
    fn entry(id: u64) -> ContextEntry {
        synthetic_entry(
            ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                content: ContentRef::text(BlobRef::from_bytes(b"history")),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: Some(TokenEstimate {
                    tokens: 10,
                    quality: TokenEstimateQuality::Estimated,
                }),
            },
            id,
        )
    }
    fn request() -> ContextCompactionRequest {
        ContextCompactionRequest {
            session_id: SessionId::new("chunked"),
            request: ContextCompactionTask {
                model: ModelSelection {
                    api_kind: ProviderApiKind::OpenAiCompletions,
                    provider_id: "custom".into(),
                    model: "unknown".into(),
                },
                request_fingerprint: "test".into(),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiCompletions,
                    context_revision: 9,
                    entries: (1..=6).map(entry).collect(),
                    token_estimate: None,
                },
                target_tokens: None,
                params: None,
                tools: vec![],
                input_limit_tokens: None,
                covered_entry_ids: (1..=6).map(ContextEntryId::new).collect(),
            },
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn typed_overflow_reduces_chunks_and_rolls_one_summary_forward() {
        let adapter = BoundedAdapter {
            seen: Mutex::new(vec![]),
            reject_after_success: false,
        };
        let result = compact(&adapter, request()).await.unwrap();
        let seen = adapter.seen.lock().unwrap();
        assert_eq!(result.calls as usize, seen.len());
        assert!(result.calls < MAX_CALLS as u32);
        assert_eq!(result.usage.as_ref().unwrap().input_tokens, Some(30));
        assert_eq!(result.context_revision, 9);
        assert_eq!(result.context_entries.len(), 1);
        assert!(
            seen.iter()
                .any(|entries| entries.iter().any(|entry| entry.entry_id.as_u64() > 100))
        );
        let covered: BTreeSet<_> = seen
            .iter()
            .filter(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.entry_id.as_u64() < 100)
                    .count()
                    <= 2
            })
            .flat_map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.entry_id.as_u64() < 100)
                    .map(|entry| entry.entry_id.as_u64())
            })
            .collect();
        assert_eq!(covered, (1..=6).collect());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_rejection_aborts_without_returning_partial_replacement() {
        let adapter = BoundedAdapter {
            seen: Mutex::new(vec![]),
            reject_after_success: true,
        };
        let error = compact(&adapter, request()).await.unwrap_err();
        assert!(
            matches!(error, LlmAdapterError::InvalidProviderRequest { message } if message == "ordinary invalid request")
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn known_capacity_splits_before_sending_and_smallest_chunk_is_bounded() {
        let adapter = BoundedAdapter {
            seen: Mutex::new(vec![]),
            reject_after_success: false,
        };
        let mut request = request();
        request.request.input_limit_tokens = Some(30);
        let result = compact(&adapter, request).await.unwrap();
        assert_eq!(result.calls, 3);
        let mut request = self::request();
        request.request.input_limit_tokens = Some(1);
        assert!(compact(&adapter, request).await.is_err());
        assert_eq!(adapter.seen.lock().unwrap().len(), 3);
    }
    #[test]
    fn chunk_boundaries_keep_parallel_tool_calls_and_results_together() {
        let mut entries: Vec<_> = (1..=5).map(entry).collect();
        entries[0].kind = ContextEntryKind::ToolCall {
            call_id: ToolCallId::new("a"),
            name: ToolName::new("tool"),
        };
        entries[1].kind = ContextEntryKind::ToolCall {
            call_id: ToolCallId::new("b"),
            name: ToolName::new("tool"),
        };
        entries[2].kind = ContextEntryKind::ToolResult {
            call_id: ToolCallId::new("a"),
            is_error: false,
        };
        entries[3].kind = ContextEntryKind::ToolResult {
            call_id: ToolCallId::new("b"),
            is_error: false,
        };
        assert_eq!(safe_cuts(&entries), vec![0, 4, 5]);
        assert_eq!(safe_cuts(&entries[..3]), vec![0]);
    }
    #[test]
    fn prior_native_compacted_window_cannot_be_split() {
        let mut entries: Vec<_> = (1..=4).map(entry).collect();
        for entry in &mut entries[..3] {
            entry.source = ContextEntrySource::Runtime {
                label: engine::STANDALONE_COMPACTION_SOURCE.into(),
            };
        }
        assert_eq!(safe_cuts(&entries), vec![0, 3, 4]);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn rolling_compaction_stops_at_the_call_budget() {
        let adapter = BoundedAdapter {
            seen: Mutex::new(vec![]),
            reject_after_success: false,
        };
        let mut request = request();
        request.request.context.entries = (1..=80).map(entry).collect();
        request.request.covered_entry_ids = (1..=80).map(ContextEntryId::new).collect();
        let error = compact(&adapter, request).await.unwrap_err();
        assert!(
            matches!(error, LlmAdapterError::InvalidProviderRequest { message } if message.contains("call budget") || message.contains("32-call budget"))
        );
        assert_eq!(adapter.seen.lock().unwrap().len(), MAX_CALLS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn input_token_budget_stops_before_an_unbounded_provider_call() {
        let adapter = BoundedAdapter {
            seen: Mutex::new(vec![]),
            reject_after_success: false,
        };
        let mut request = request();
        request.request.context.entries[0]
            .token_estimate
            .as_mut()
            .unwrap()
            .tokens = MAX_INPUT_TOKENS as u32 + 1;
        let error = compact(&adapter, request).await.unwrap_err();
        assert!(
            matches!(error, LlmAdapterError::InvalidProviderRequest { message } if message.contains("input token budget"))
        );
        assert!(adapter.seen.lock().unwrap().is_empty());
    }
}
