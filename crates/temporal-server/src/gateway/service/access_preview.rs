//! Advisory permissions for the current caller. No permission grant or audit
//! admission is created for the actions being inspected.
use super::*;

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
            ResourceRef::Collection(id) => api::validate_session_id(id).is_ok(),
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
        // Never accept a principal selector or internal-controller fallback.
        let rights = self.caller()?.rights;
        let store = self.access_store();
        let mut resources = Vec::with_capacity(params.resources.len());
        for resource in params.resources {
            let actions = store
                .resource_actions(&rights, &resource, params.session_delete_cascade)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
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
            vec![Read, CreateSession, CreateProfile, CreateBot, UseResource]
        );
        let mut admin = rights(access::Role::Admin);
        assert_eq!(
            universe_actions(&admin),
            vec![
                Read,
                CreateSession,
                CreateProfile,
                CreateBot,
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
}
