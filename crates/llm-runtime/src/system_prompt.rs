//! The system prompt, built the same way for every provider.
//!
//! The harness plans instruction entries first, ordered by key so the most
//! stable layers come first. Every adapter joins them into a single text: one
//! Anthropic system block, the Responses `instructions` field, or one leading
//! Chat Completions `system`/`developer` message. Self-hosted chat templates
//! (Qwen among them) accept at most one system message, and only as the first
//! message, so a single text is also the one shape every template renders.
//!
//! Joining keeps the layers in order, so a prompt prefix cache behaves as it
//! would with one message per layer: a change in one layer invalidates that
//! layer and everything after it, nothing before it.

use harness::{ContextEntry, ContextEntryKind, storage::BlobStore};

use crate::{blob_io::read_text, error::LlmAdapterResult};

/// Separator between instruction layers.
const LAYER_SEPARATOR: &str = "\n\n";

/// All instruction entries in planned order, each trimmed, empty ones
/// skipped, joined by a blank line. `None` when no layer has text, since
/// providers reject an empty system prompt.
pub(crate) async fn collated_instructions(
    blobs: &dyn BlobStore,
    entries: &[ContextEntry],
) -> LlmAdapterResult<Option<String>> {
    let mut layers = Vec::new();
    for entry in entries {
        if matches!(entry.kind, ContextEntryKind::Instructions) {
            let text = read_text(blobs, &entry.content.content_ref).await?;
            let text = text.trim();
            if !text.is_empty() {
                layers.push(text.to_owned());
            }
        }
    }
    Ok((!layers.is_empty()).then(|| layers.join(LAYER_SEPARATOR)))
}
