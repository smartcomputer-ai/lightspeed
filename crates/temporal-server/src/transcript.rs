//! Shared audio transcript provenance format.
//!
//! The preprocess activity commits transcripts as `[audio transcript: {name}]`
//! followed by the transcript body, tagging the entry with the provider kind
//! below and the source audio blob ref as `provider_item_id`. The gateway
//! detects and unwraps that format when returning append activation text, so
//! both sides must agree on it here.

pub(crate) const AUDIO_TRANSCRIPT_PROVIDER_KIND: &str = "lightspeed.audio.transcript";

const AUDIO_TRANSCRIPT_HEADER_PREFIX: &str = "[audio transcript:";

pub(crate) fn transcript_header(name: &str) -> String {
    format!("{AUDIO_TRANSCRIPT_HEADER_PREFIX} {name}]")
}

pub(crate) fn transcript_content(name: &str, text: &str) -> String {
    format!("{}\n{}", transcript_header(name), text.trim())
}

/// Strips the transcript header line, returning the raw transcript body.
pub(crate) fn transcript_activation_text(text: &str) -> &str {
    let text = text.trim();
    if let Some((first, rest)) = text.split_once('\n')
        && first
            .trim_start()
            .starts_with(AUDIO_TRANSCRIPT_HEADER_PREFIX)
    {
        return rest.trim();
    }
    text
}
