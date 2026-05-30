//! Model and provider target for model-visible tool shaping.

use engine::{ModelSelection, ProviderApiKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolTarget {
    pub api_kind: ProviderApiKind,
    pub provider_id: String,
    pub model: String,
}

impl ToolTarget {
    pub fn new(
        api_kind: ProviderApiKind,
        provider_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_kind,
            provider_id: provider_id.into(),
            model: model.into(),
        }
    }

    pub fn api_kind(api_kind: ProviderApiKind) -> Self {
        Self::new(api_kind, "", "")
    }
}

impl From<&ModelSelection> for ToolTarget {
    fn from(model: &ModelSelection) -> Self {
        Self {
            api_kind: model.api_kind.clone(),
            provider_id: model.provider_id.clone(),
            model: model.model.clone(),
        }
    }
}
