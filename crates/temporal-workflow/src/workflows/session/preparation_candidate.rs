//! Private preparation state. Commands are admitted and reduced locally; only
//! a fully prepared candidate can become one durable event batch.
use super::*;
use api::AgentApiError;
use engine::storage::{StoredSessionEntry, UncommittedStoredEvent};

pub(super) struct PreparationCandidate {
    drive: CoreAgentDrive,
    original_head: Option<SessionPosition>,
    events: Vec<UncommittedStoredEvent>,
}

impl PreparationCandidate {
    pub(super) fn new(live: &CoreAgentDrive) -> Self {
        Self {
            drive: CoreAgentDrive::from_replayed(
                live.session_id().clone(),
                live.state().clone(),
                live.head().cloned(),
            ),
            original_head: live.head().cloned(),
            events: Vec::new(),
        }
    }

    pub(super) fn state(&self) -> &CoreAgentState {
        self.drive.state()
    }

    pub(super) fn push(
        &mut self,
        command: CoreAgentCommand,
        now: u64,
    ) -> Result<(), AgentApiError> {
        self.push_command(command, now)?;
        // Include the same source invalidations as ordinary publication, so
        // commit requires no second append and refresh sees the proposed sources.
        for invalidation in [
            drive::invalid_environment_prompt_command,
            drive::invalid_environment_catalog_command,
            drive::invalid_vfs_skill_catalog_command,
        ] {
            if let Some(command) = invalidation(self.state()) {
                self.push_command(command, now)?;
            }
        }
        Ok(())
    }

    fn push_command(&mut self, command: CoreAgentCommand, now: u64) -> Result<(), AgentApiError> {
        if drive::environment_prompt_publication_is_obsolete(self.state(), &command)
            || drive::environment_catalog_publication_is_obsolete(self.state(), &command)
            || drive::vfs_skill_catalog_publication_is_obsolete(self.state(), &command)
        {
            return Err(AgentApiError::conflict(
                "context observation does not match the proposed session sources",
            ));
        }
        match self
            .drive
            .admit_command(command, now)
            .map_err(map_candidate_error)?
        {
            CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => {
                let mut seq = expected_head.as_ref().map_or(0, |head| head.seq.as_u64());
                let entries = events
                    .iter()
                    .map(|event| {
                        seq = seq.checked_add(1).ok_or_else(|| {
                            AgentApiError::internal("session event sequence exhausted")
                        })?;
                        Ok(StoredSessionEntry {
                            position: SessionPosition {
                                seq: engine::EventSeq::new(seq),
                            },
                            observed_at_ms: event.observed_at_ms,
                            joins: event.joins.clone(),
                            event: event.event.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>, AgentApiError>>()?;
                self.drive
                    .resume_appended(entries)
                    .map_err(map_candidate_error)?;
                self.events.extend(events);
                Ok(())
            }
            CoreAgentAction::Idle | CoreAgentAction::Closed => Ok(()),
            _ => Err(AgentApiError::internal(
                "preparation command produced a runtime action",
            )),
        }
    }

    pub(super) fn tools(
        &mut self,
        prepared: crate::SessionToolsetPreparation,
        universe_id: uuid::Uuid,
        now: u64,
    ) -> Result<(), AgentApiError> {
        for declaration in prepared.declarations {
            self.push(
                CoreAgentCommand::AdmitSystemWorkflowTool {
                    session_universe_id: universe_id,
                    declaration,
                },
                now,
            )?;
        }
        let patch = crate::session_toolset_patch(&self.state().tooling.tools, &prepared.tools);
        if !patch.is_empty() {
            self.push(
                CoreAgentCommand::PatchTools {
                    expected_revision: Some(self.state().tooling.revision),
                    patch,
                },
                now,
            )?;
        }
        Ok(())
    }

    pub(super) fn finish(
        self,
        live: &CoreAgentDrive,
    ) -> Result<AppendEventsRequest, AgentApiError> {
        if self.drive.session_id() != live.session_id()
            || self.original_head.as_ref() != live.head()
        {
            return Err(AgentApiError::conflict(
                "session changed during preparation",
            ));
        }
        Ok(AppendEventsRequest {
            session_id: live.session_id().clone(),
            expected_head: self.original_head,
            events: self.events,
        })
    }
}

fn map_candidate_error(error: CoreAgentDriveError) -> AgentApiError {
    match error {
        CoreAgentDriveError::Command(CommandError::Rejected(rejection)) => {
            AgentApiError::rejected(rejection.to_string())
        }
        error => AgentApiError::internal(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(live: &mut CoreAgentDrive, request: AppendEventsRequest) {
        assert_eq!(live.head(), request.expected_head.as_ref());
        let start = live.head().map_or(0, |head| head.seq.as_u64());
        let entries = request
            .events
            .into_iter()
            .enumerate()
            .map(|(index, event)| StoredSessionEntry {
                position: SessionPosition {
                    seq: engine::EventSeq::new(start + index as u64 + 1),
                },
                observed_at_ms: event.observed_at_ms,
                joins: event.joins,
                event: event.event,
            })
            .collect();
        live.resume_appended(entries).unwrap();
    }

    fn live() -> CoreAgentDrive {
        let mut live = CoreAgentDrive::from_replayed(
            SessionId::new("atomic-preparation"),
            CoreAgentState::new(),
            None,
        );
        let mut candidate = PreparationCandidate::new(&live);
        candidate
            .push(
                CoreAgentCommand::OpenSession {
                    config: crate::default_session_config(engine::ModelSelection {
                        api_kind: engine::ProviderApiKind::OpenAiResponses,
                        provider_id: "openai".into(),
                        model: "test-model".into(),
                    }),
                },
                1,
            )
            .unwrap();
        let batch = candidate.finish(&live).unwrap();
        commit(&mut live, batch);
        live
    }

    fn instructions(text: &str) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Instructions,
            content: engine::ContentRef::text(BlobRef::from_bytes(text.as_bytes())),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    fn proposed(live: &CoreAgentDrive) -> PreparationCandidate {
        let mut candidate = PreparationCandidate::new(live);
        let tool = engine::ToolSpec {
            name: engine::ToolName::new("new_tool"),
            kind: engine::ToolKind::Function(engine::FunctionToolSpec {
                description_ref: None,
                input_schema_ref: BlobRef::from_bytes(b"schema"),
                output_schema_ref: None,
                strict: None,
                provider_options_ref: None,
            }),
            parallelism: engine::ToolParallelism::ParallelSafe,
            execution: Default::default(),
        };
        let tools = BTreeMap::from([(tool.name.clone(), tool)]);
        candidate
            .tools(
                crate::SessionToolsetPreparation {
                    source: crate::SessionToolsetSource::from_state(live.state()).unwrap(),
                    declarations: Vec::new(),
                    tools,
                },
                uuid::Uuid::nil(),
                2,
            )
            .unwrap();
        let mut config = live.state().lifecycle.config.clone().unwrap();
        config.features.environments = Some(engine::EnvironmentsFeature::default());
        config.generation.tool_choice = Some(engine::ToolChoice::Specific {
            tool_name: engine::ToolName::new("new_tool"),
        });
        candidate
            .push(
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: Some(live.state().lifecycle.config_revision),
                    config,
                },
                2,
            )
            .unwrap();
        candidate
            .push(
                CoreAgentCommand::SetActiveEnvironment {
                    environment_id: engine::EnvironmentId::new("selected"),
                },
                2,
            )
            .unwrap();
        candidate
            .push(
                CoreAgentCommand::ReplaceContextPrefix {
                    expected_revision: None,
                    key_prefix: ContextEntryKey::new("instructions"),
                    entries: BTreeMap::from([(
                        ContextEntryKey::new("instructions.050.profile"),
                        instructions("new profile"),
                    )]),
                },
                2,
            )
            .unwrap();
        candidate
    }

    #[test]
    fn preparation_observes_proposed_sources_and_publishes_one_replayable_batch() {
        let mut live = live();
        let before = live.state().clone();
        let mut candidate = proposed(&live);
        let request = admissions::runtime_projection_request(live.session_id(), candidate.state());
        assert!(request.environments.is_some());
        assert_eq!(
            request.active_environment_id,
            Some(engine::EnvironmentId::new("selected"))
        );
        assert_eq!(
            request.active_instruction_inputs[&ContextEntryKey::new("instructions.050.profile")],
            instructions("new profile")
        );
        assert_eq!(live.state(), &before);
        candidate
            .push(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("runtime.catalog.prepared"),
                    entry: ContextEntryInput {
                        kind: ContextEntryKind::Catalog {
                            title: "Prepared catalog".into(),
                        },
                        ..instructions("new catalog")
                    },
                },
                3,
            )
            .unwrap();
        let expected = candidate.state().clone();
        let batch = candidate.finish(&live).unwrap();
        assert_eq!(batch.expected_head.as_ref(), live.head());
        assert!(batch.events.len() >= 5);
        assert_eq!(live.state(), &before);
        commit(&mut live, batch);
        assert_eq!(live.state(), &expected);
    }

    #[test]
    fn late_validation_failure_discards_all_prepared_changes() {
        let live = live();
        let before = live.state().clone();
        let mut candidate = proposed(&live);
        let error = candidate
            .push(
                CoreAgentCommand::UpsertContext {
                    expected_revision: Some(999),
                    key: ContextEntryKey::new("runtime.catalog.prepared"),
                    entry: ContextEntryInput {
                        kind: ContextEntryKind::Catalog {
                            title: "Prepared catalog".into(),
                        },
                        ..instructions("bad refresh")
                    },
                },
                3,
            )
            .unwrap_err();
        assert_eq!(error.kind, api::AgentApiErrorKind::Rejected);
        drop(candidate);
        assert_eq!(live.state(), &before);
        // A later valid attempt can still prepare from exactly the original state.
        assert!(proposed(&live).finish(&live).is_ok());
    }

    #[test]
    fn concurrent_close_or_revision_change_rejects_the_entire_candidate() {
        for close in [false, true] {
            let mut live = live();
            let candidate = proposed(&live);
            let mut concurrent = PreparationCandidate::new(&live);
            let command = if close {
                CoreAgentCommand::CloseSession { force: true }
            } else {
                let mut config = live.state().lifecycle.config.clone().unwrap();
                config.model.model = "other-model".into();
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: None,
                    config,
                }
            };
            concurrent.push(command, 3).unwrap();
            let batch = concurrent.finish(&live).unwrap();
            commit(&mut live, batch);
            let after_concurrent = live.state().clone();
            assert_eq!(
                candidate.finish(&live).unwrap_err().kind,
                api::AgentApiErrorKind::Conflict
            );
            assert_eq!(live.state(), &after_concurrent);
            assert!(live.state().tooling.tools.is_empty());
            assert!(live.state().environment.active_environment_id.is_none());
        }
    }

    #[test]
    fn source_invalidation_is_part_of_the_batch_and_obsolete_publication_is_rejected() {
        let mut live = live();
        let mut initial = PreparationCandidate::new(&live);
        let mut config = live.state().lifecycle.config.clone().unwrap();
        config.features.environments = Some(engine::EnvironmentsFeature {
            prompts: Some(Default::default()),
            ..Default::default()
        });
        initial
            .push(
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: None,
                    config,
                },
                2,
            )
            .unwrap();
        initial
            .push(
                CoreAgentCommand::SetActiveEnvironment {
                    environment_id: engine::EnvironmentId::new("old"),
                },
                2,
            )
            .unwrap();
        let old = ContextEntryInput {
            origin: Some("runtime.environment:old".into()),
            ..instructions("old machine prompt")
        };
        initial
            .push(
                CoreAgentCommand::ReplaceContextPrefix {
                    expected_revision: None,
                    key_prefix: ContextEntryKey::new("instructions"),
                    entries: BTreeMap::from([(
                        ContextEntryKey::new("instructions.110.environment"),
                        old.clone(),
                    )]),
                },
                2,
            )
            .unwrap();
        let batch = initial.finish(&live).unwrap();
        commit(&mut live, batch);
        let mut candidate = PreparationCandidate::new(&live);
        candidate
            .push(
                CoreAgentCommand::SetActiveEnvironment {
                    environment_id: engine::EnvironmentId::new("new"),
                },
                3,
            )
            .unwrap();
        assert!(
            !admissions::runtime_projection_request(live.session_id(), candidate.state())
                .active_instruction_inputs
                .contains_key(&ContextEntryKey::new("instructions.110.environment"))
        );
        assert!(drive::invalid_environment_prompt_command(candidate.state()).is_none());
        let batch = candidate.finish(&live).unwrap();
        assert_eq!(batch.events.len(), 2);
        commit(&mut live, batch);
        assert!(drive::invalid_environment_prompt_command(live.state()).is_none());
        let mut candidate = PreparationCandidate::new(&live);
        let error = candidate
            .push(
                CoreAgentCommand::ReplaceContextPrefix {
                    expected_revision: None,
                    key_prefix: ContextEntryKey::new("instructions"),
                    entries: BTreeMap::from([(
                        ContextEntryKey::new("instructions.110.environment"),
                        old,
                    )]),
                },
                4,
            )
            .unwrap_err();
        assert_eq!(error.kind, api::AgentApiErrorKind::Conflict);
    }
}
