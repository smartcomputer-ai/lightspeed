use agent_core::{ContextItem, ProviderApiKind, ResolvedContextWindow, TokenEstimate};

pub fn resolved_context_window(
    api_kind: ProviderApiKind,
    items: impl IntoIterator<Item = ContextItem>,
    token_estimate: Option<TokenEstimate>,
) -> ResolvedContextWindow {
    ResolvedContextWindow {
        api_kind,
        items: items.into_iter().collect(),
        token_estimate,
    }
}
