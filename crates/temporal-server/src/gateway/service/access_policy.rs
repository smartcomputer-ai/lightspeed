//! Sharing: the policy of a root and its replacement. A resource below a root
//! shows and shares its root's policy. The handler admits the caller by the
//! evaluator's `ShareResource`; the store decides the replacement itself
//! again under the policy row's lock, from the actor's rights read after
//! the lock, with the finer rules (only the owner hands over or grants
//! write, grants fit the kind), so a right revoked in between cannot commit.
use super::*;
use access::{ResourcePermission, Subject, Visibility};

const MAX_GRANTS: usize = 200;

fn policy_view(resource: ResourceRef, record: store_pg::ResourcePolicyRecord) -> AccessPolicyView {
    AccessPolicyView {
        resource,
        root: record.anchor.audience_root,
        owner: record.policy.owner,
        visibility: record.policy.visibility,
        execution: record.anchor.execution,
        grants: record.grants,
        revision: record.policy.revision,
        updated_by: record.policy.updated_by,
        updated_at_ms: record.policy.updated_at_ms,
    }
}

fn map_policy_error(resource: &ResourceRef, error: access::AccessError) -> AgentApiError {
    match error {
        access::AccessError::Invalid(message) => AgentApiError::invalid_request(message),
        access::AccessError::NotFound => authorization::not_found(resource),
        access::AccessError::RevisionMismatch { expected, actual } => AgentApiError::conflict(
            format!("access policy revision is {actual}, expected {expected}"),
        ),
        access::AccessError::Denied => AgentApiError::forbidden(),
        other => AgentApiError::internal(other.to_string()),
    }
}

/// Shape a grant set; the store validates it against the root.
fn grants_from_input(
    grants: &[AccessGrantInput],
) -> Result<Vec<(Subject, ResourcePermission)>, AgentApiError> {
    if grants.len() > MAX_GRANTS {
        return Err(AgentApiError::invalid_request(format!(
            "at most {MAX_GRANTS} grants may be set"
        )));
    }
    Ok(grants
        .iter()
        .map(|grant| (grant.subject, grant.permission))
        .collect())
}

impl GatewayAgentApi {
    pub(super) async fn read_access_policy_record(
        &self,
        params: AccessPolicyReadParams,
    ) -> Result<AccessPolicyReadResponse, AgentApiError> {
        let resource = params.resource;
        let record = self
            .access_store()
            .read_policy(self.universe_id(), &resource)
            .await
            .map_err(|error| map_policy_error(&resource, error))?
            .ok_or_else(|| authorization::not_found(&resource))?;
        Ok(AccessPolicyReadResponse {
            policy: policy_view(resource, record),
        })
    }

    /// Replace visibility and grants of the resource's root, and hand it
    /// over when the request names a new owner.
    pub(super) async fn put_access_policy_record(
        &self,
        params: AccessPolicyPutParams,
    ) -> Result<AccessPolicyPutResponse, AgentApiError> {
        let actor = self.caller()?.acting_principal().id;
        let store = self.access_store();
        let resource = params.resource;
        let root = store
            .anchor(self.universe_id(), &resource)
            .await
            .map_err(|error| map_policy_error(&resource, error))?
            .ok_or_else(|| authorization::not_found(&resource))?
            .audience_root;
        let record = store
            .put_policy(
                self.universe_id(),
                actor,
                &root,
                &access::PolicyReplacement {
                    visibility: params.visibility,
                    grants: grants_from_input(&params.grants)?,
                    owner: params.owner,
                },
                params.expected_revision,
                now_ms()? as u64,
            )
            .await
            .map_err(|error| map_policy_error(&resource, error))?;
        Ok(AccessPolicyPutResponse {
            policy: policy_view(resource, record),
        })
    }

    /// Apply a creation-time audience to a freshly reserved root. The
    /// reservation made it universe-visible (or, for a workspace, environment
    /// or MCP server, the requested visibility) with no grants; this
    /// replaces that before the resource is usable, and the store refuses
    /// grants that do not fit the kind. A resource that is not a root
    /// refuses an audience of its own.
    pub(super) async fn apply_creation_access(
        &self,
        resource: &ResourceRef,
        access: Option<AccessInput>,
    ) -> Result<(), AgentApiError> {
        let store = self.access_store();
        // A personal root is restricted unless its owner says otherwise.
        let personal = store
            .anchor(self.universe_id(), resource)
            .await
            .map_err(|error| map_policy_error(resource, error))?
            .and_then(|anchor| anchor.execution)
            .is_some_and(|execution| execution.kind == access::ExecutionKind::Personal);
        let access = match access {
            Some(access) => access,
            None if personal => AccessInput {
                visibility: Some(Visibility::Restricted),
                ..AccessInput::default()
            },
            None => return Ok(()),
        };
        let current = store
            .read_policy(self.universe_id(), resource)
            .await
            .map_err(|error| map_policy_error(resource, error))?
            .ok_or_else(|| authorization::not_found(resource))?;
        if current.anchor.audience_root != *resource {
            return Err(AgentApiError::invalid_request(
                "a resource created under another root has no audience of its own",
            ));
        }
        store
            .put_policy(
                self.universe_id(),
                self.caller()?.acting_principal().id,
                resource,
                &access::PolicyReplacement {
                    visibility: access.visibility.unwrap_or(if personal {
                        Visibility::Restricted
                    } else {
                        Visibility::Universe
                    }),
                    grants: grants_from_input(&access.grants)?,
                    owner: None,
                },
                None,
                now_ms()? as u64,
            )
            .await
            .map_err(|error| map_policy_error(resource, error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_sets_are_bounded() {
        let grant = AccessGrantInput {
            subject: Subject::Principal(uuid::Uuid::new_v4()),
            permission: ResourcePermission::Use,
        };
        assert_eq!(
            grants_from_input(&vec![grant.clone(); MAX_GRANTS])
                .unwrap()
                .len(),
            MAX_GRANTS
        );
        assert_eq!(
            grants_from_input(&vec![grant; MAX_GRANTS + 1])
                .unwrap_err()
                .kind,
            AgentApiErrorKind::InvalidRequest
        );
    }
}
