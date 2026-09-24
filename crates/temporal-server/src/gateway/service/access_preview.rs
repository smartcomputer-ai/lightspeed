//! Advisory permissions for the current caller, or for the universe's
//! execution service on what the caller may see. No permission grant or
//! audit admission is created for the actions being inspected.
use super::*;
use access::{AccessStore as _, Caller, Decision};

const MAX_ACCESS_RESOURCES: usize = 100;

fn validate(params: &AccessReadParams) -> Result<(), AgentApiError> {
    if params.resources.len() > MAX_ACCESS_RESOURCES {
        return Err(AgentApiError::invalid_request(
            "at most 100 resources may be inspected",
        ));
    }
    for resource in &params.resources {
        let valid = match resource {
            ResourceRef::Session(id) => api::validate_session_id(id).is_ok(),
            ResourceRef::Bot(id) => api::BotId::try_new(id.clone()).is_ok(),
            ResourceRef::Profile(id) => api::ProfileId::try_new(id.clone()).is_ok(),
            ResourceRef::Workspace(id) => VfsWorkspaceId::try_new(id.clone()).is_ok(),
            ResourceRef::Environment(id) => {
                ::environments::EnvironmentId::try_new(id.clone()).is_ok()
            }
            ResourceRef::McpServer(id) => mcp::McpServerId::try_new(id.clone()).is_ok(),
        };
        if !valid {
            return Err(AgentApiError::invalid_request("invalid resource id"));
        }
    }
    Ok(())
}

fn universe_actions(rights: &access::EffectiveAccess) -> Vec<UniverseAction> {
    use UniverseAction::*;
    [
        Read,
        CreateSession,
        CreateProfile,
        CreateBot,
        CreateWorkspace,
        UseResource,
        ConfigureResource,
        ManageAccess,
    ]
    .into_iter()
    .filter(|action| rights.universe_action(*action) == access::RoleDecision::Allowed)
    .collect()
}

impl GatewayAgentApi {
    pub(super) async fn read_action_permissions(
        &self,
        params: AccessReadParams,
    ) -> Result<AccessReadResponse, AgentApiError> {
        validate(&params)?;
        // Never accept a principal selector or internal-controller fallback:
        // the only other identity is the universe's own execution service.
        let rights = self.caller()?.rights;
        let store = self.access_store();
        let internal = |error: access::AccessError| AgentApiError::internal(error.to_string());
        let service = match params.decide_as {
            AccessReadAs::Caller => None,
            AccessReadAs::ExecutionService => {
                let policy = store
                    .universe_execution_policy(self.universe_id(), now_ms()? as u64)
                    .await
                    .map_err(internal)?;
                Some(
                    store
                        .effective_access(policy.execution_principal_id, rights.scope)
                        .await
                        .map_err(internal)?,
                )
            }
        };
        let mut resources = Vec::with_capacity(params.resources.len());
        for resource in params.resources {
            let actions = match &service {
                None => store
                    .resource_actions(&rights, &resource, params.session_delete_cascade)
                    .await
                    .map_err(internal)?,
                // Only what the caller may see is decided for the service,
                // so the preview discloses nothing the caller cannot read.
                Some(service) => {
                    let visible = store
                        .decide(Caller::Request(&rights), UniverseAction::Read, &resource)
                        .await
                        .map_err(internal)?
                        .is_some_and(Decision::allows);
                    if visible {
                        store
                            .resource_actions(service, &resource, params.session_delete_cascade)
                            .await
                            .map_err(internal)?
                    } else {
                        Vec::new()
                    }
                }
            };
            resources.push(ResourceAccessView { resource, actions });
        }
        Ok(AccessReadResponse {
            actions: universe_actions(&rights),
            resources,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rights(role: access::Role) -> access::EffectiveAccess {
        access::EffectiveAccess {
            principal: access::Principal {
                id: uuid::Uuid::new_v4(),
                kind: access::PrincipalKind::User,
                status: access::PrincipalStatus::Active,
                display_name: "Reader".into(),
                management_scope: access::AccessScope::Deployment,
                created_at_ms: 0,
            },
            scope: access::AccessScope::Universe {
                universe_id: uuid::Uuid::new_v4(),
            },
            roles: [role].into_iter().collect(),
            capabilities: Default::default(),
            policy_revision: 1,
        }
    }

    #[test]
    fn broad_roles_never_advertise_target_actions_globally() {
        use UniverseAction::*;
        assert_eq!(universe_actions(&rights(access::Role::Viewer)), vec![Read]);
        assert_eq!(
            universe_actions(&rights(access::Role::Contributor)),
            vec![
                Read,
                CreateSession,
                CreateProfile,
                CreateBot,
                CreateWorkspace,
                UseResource
            ]
        );
        // An agent identity sees and uses; it creates and configures nothing.
        assert_eq!(
            universe_actions(&rights(access::Role::Executor)),
            vec![Read, UseResource]
        );
        let mut admin = rights(access::Role::Admin);
        assert_eq!(
            universe_actions(&admin),
            vec![
                Read,
                CreateSession,
                CreateProfile,
                CreateBot,
                CreateWorkspace,
                UseResource,
                ConfigureResource,
                ManageAccess
            ]
        );
        admin.principal.status = access::PrincipalStatus::Disabled;
        assert!(universe_actions(&admin).is_empty());
        assert!(universe_actions(&rights(access::Role::DeploymentAdmin)).is_empty());
    }

    #[test]
    fn preview_is_bounded_and_cannot_select_another_actor() {
        let mut params = AccessReadParams::default();
        assert!(validate(&params).is_ok());
        params.resources = vec![ResourceRef::Session("session".into()); 101];
        assert_eq!(
            validate(&params).unwrap_err().kind,
            AgentApiErrorKind::InvalidRequest
        );
        params.resources = vec![ResourceRef::Session("foreign/session".into())];
        assert_eq!(
            validate(&params).unwrap_err().kind,
            AgentApiErrorKind::InvalidRequest
        );
        assert!(
            serde_json::from_value::<AccessReadParams>(
                serde_json::json!({"principalId": uuid::Uuid::new_v4()})
            )
            .is_err()
        );
        assert!(
            !serde_json::from_value::<AccessReadParams>(serde_json::json!({}))
                .unwrap()
                .session_delete_cascade
        );
    }

    #[test]
    fn preview_decides_for_the_caller_unless_the_execution_service_is_asked_for() {
        let parse = |value| serde_json::from_value::<AccessReadParams>(value);
        assert_eq!(
            parse(serde_json::json!({})).unwrap().decide_as,
            AccessReadAs::Caller
        );
        let params = parse(serde_json::json!({
            "resources": [{"kind": "environment", "id": "environment_1"}],
            "as": "execution_service",
        }))
        .unwrap();
        assert_eq!(params.decide_as, AccessReadAs::ExecutionService);
        assert!(validate(&params).is_ok());
        // Only the universe's own execution service can be named.
        assert!(parse(serde_json::json!({"as": "principal"})).is_err());
        assert!(parse(serde_json::json!({"as": {"principal": uuid::Uuid::new_v4()}})).is_err());
    }

    #[test]
    fn preview_validates_ids_of_every_kind() {
        let invalid = |resource| {
            validate(&AccessReadParams {
                resources: vec![resource],
                ..AccessReadParams::default()
            })
            .unwrap_err()
            .kind
        };
        for resource in [
            ResourceRef::Workspace("foreign/workspace".into()),
            ResourceRef::Environment("foreign/environment".into()),
            ResourceRef::McpServer("foreign/server".into()),
        ] {
            assert_eq!(invalid(resource), AgentApiErrorKind::InvalidRequest);
        }
        assert!(
            validate(&AccessReadParams {
                resources: vec![
                    ResourceRef::Workspace("workspace_1".into()),
                    ResourceRef::Environment("environment_1".into()),
                    ResourceRef::McpServer("github".into()),
                ],
                ..AccessReadParams::default()
            })
            .is_ok()
        );
    }
}
