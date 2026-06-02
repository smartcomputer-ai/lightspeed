use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{BlobRef, CoreAgentState, DomainError, SkillId, ToolCallId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    CatalogSet {
        catalog: Option<SkillCatalogContext>,
    },
    ActivationsSet {
        activations: Vec<SkillActivation>,
    },
}

pub type SkillEvent = Event;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillState {
    pub catalog: Option<SkillCatalogContext>,
    /// Skill bodies currently eligible for request planning.
    ///
    /// Historical skill injections are represented by context items in the log;
    /// once an activation stops being active, remove it from this list.
    pub activations: Vec<SkillActivation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogContext {
    pub catalog_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillActivation {
    pub skill_id: SkillId,
    pub catalog_ref: BlobRef,
    pub source: SkillActivationSource,
    pub scope: SkillActivationScope,
}

impl SkillActivation {
    pub fn direct_context_ref(&self) -> Option<&BlobRef> {
        match &self.source {
            SkillActivationSource::DirectContext { context_ref } => Some(context_ref),
            SkillActivationSource::ToolResult { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationSource {
    ToolResult { call_id: ToolCallId },
    DirectContext { context_ref: BlobRef },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationScope {
    Run,
    Session,
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::CatalogSet { catalog } => {
            state.skills.catalog = catalog.clone();
            Ok(())
        }
        Event::ActivationsSet { activations } => {
            validate_activations(activations)?;
            state.skills.activations = activations.clone();
            Ok(())
        }
    }
}

pub(crate) fn validate_activations(activations: &[SkillActivation]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for activation in activations {
        if !seen.insert(&activation.skill_id) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate active skill activation {}",
                activation.skill_id
            )));
        }
    }
    Ok(())
}
