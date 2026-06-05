use engine::{ContextEntry, ContextSnapshot, ProviderApiKind, TokenEstimate};

pub fn context_snapshot(
    api_kind: ProviderApiKind,
    entries: impl IntoIterator<Item = ContextEntry>,
    token_estimate: Option<TokenEstimate>,
) -> ContextSnapshot {
    ContextSnapshot {
        api_kind,
        context_revision: 0,
        entries: entries.into_iter().collect(),
        token_estimate,
    }
}
