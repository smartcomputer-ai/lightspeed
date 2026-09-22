//! Sharing: the policy of a root and its replacement. A resource below a root
//! shows and shares its root's policy. Authorization is the evaluator's
//! `ShareResource`; the finer rules (only the owner grants write) read the
//! current policy in the handler.
use super::*;
use access::{ResourcePermission, Subject, Visibility};

const MAX_GRANTS: usize = 200;

fn policy_view(resource: ResourceRef, record: store_pg::ResourcePolicyRecord) -> AccessPolicyView {
    AccessPolicyView {
        resource,
        root: record.anchor.audience_root,
        owner: record.policy.owner,
        visibility: record.policy.visibility,
        grants: record.grants,
        revision: record.policy.revision,
        updated_by: record.policy.updated_by,
        updated_at_ms: record.policy.updated_at_ms,
    }
}

fn map_policy_error(error: access::AccessError) -> AgentApiError {
    match error {
        access::AccessError::Invalid(message) => AgentApiError::invalid_request(message),
        access::AccessError::NotFound => AgentApiError::not_found("resource not found"),
        access::AccessError::RevisionMismatch { expected, actual } => AgentApiError::conflict(
            format!("access policy revision is {actual}, expected {expected}"),
        ),
        access::AccessError::Denied => AgentApiError::forbidden(),
        other => AgentApiError::internal(other.to_string()),
    }
}

/// Validate a grant set: bounded, and the owner is not granted to itself.
pub(super) fn grants_from_input(
    owner: uuid::Uuid,
    grants: &[AccessGrantInput],
) -> Result<Vec<(Subject, ResourcePermission)>, AgentApiError> {
    if grants.len() > MAX_GRANTS {
        return Err(AgentApiError::invalid_request(format!(
            "at most {MAX_GRANTS} grants may be set"
        )));
    }
    if grants
        .iter()
        .any(|grant| grant.subject == Subject::Principal(owner))
    {
        return Err(AgentApiError::invalid_request(
            "the owner holds every permission and takes no grant",
        ));
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
        let record = self
            .access_store()
            .read_policy(self.universe_id(), &params.resource)
            .await
            .map_err(map_policy_error)?
            .ok_or_else(|| AgentApiError::not_found("resource not found"))?;
        Ok(AccessPolicyReadResponse {
            policy: policy_view(params.resource, record),
        })
    }

    /// Replace visibility and grants of the resource's root. The evaluator
    /// admitted the caller as owner or writer; a writer may not grant write,
    /// so the requested set is compared with the current one.
    pub(super) async fn put_access_policy_record(
        &self,
        params: AccessPolicyPutParams,
    ) -> Result<AccessPolicyPutResponse, AgentApiError> {
        let actor = self.caller()?.acting_principal().id;
        let store = self.access_store();
        let current = store
            .read_policy(self.universe_id(), &params.resource)
            .await
            .map_err(map_policy_error)?
            .ok_or_else(|| AgentApiError::not_found("resource not found"))?;
        let grants = grants_from_input(current.policy.owner, &params.grants)?;
        if current.policy.owner != actor {
            let widened = grants.iter().any(|(subject, permission)| {
                *permission == ResourcePermission::Write
                    && !current.grants.iter().any(|existing| {
                        existing.subject == *subject
                            && existing.permission == ResourcePermission::Write
                    })
            });
            let narrowed = current.grants.iter().any(|existing| {
                existing.permission == ResourcePermission::Write
                    && !grants.iter().any(|(subject, permission)| {
                        *subject == existing.subject && *permission == ResourcePermission::Write
                    })
            });
            if widened || narrowed {
                return Err(AgentApiError::forbidden());
            }
        }
        let root = current.anchor.audience_root.clone();
        let record = store
            .put_policy(
                self.universe_id(),
                actor,
                &root,
                &store_pg::PolicyReplacement {
                    visibility: params.visibility,
                    grants,
                    expected_revision: params.expected_revision,
                },
                now_ms()? as u64,
            )
            .await
            .map_err(map_policy_error)?;
        Ok(AccessPolicyPutResponse {
            policy: policy_view(params.resource, record),
        })
    }

    /// Apply a creation-time audience to a freshly reserved root. The
    /// reservation made it universe-visible with no grants; this replaces
    /// that before the resource is usable. A resource that is not a root
    /// refuses an audience of its own.
    pub(super) async fn apply_creation_access(
        &self,
        resource: &ResourceRef,
        access: Option<AccessInput>,
    ) -> Result<(), AgentApiError> {
        let Some(access) = access else {
            return Ok(());
        };
        let store = self.access_store();
        let current = store
            .read_policy(self.universe_id(), resource)
            .await
            .map_err(map_policy_error)?
            .ok_or_else(|| AgentApiError::not_found("resource not found"))?;
        if current.anchor.audience_root != *resource {
            return Err(AgentApiError::invalid_request(
                "a resource created under another root has no audience of its own",
            ));
        }
        let actor = self.caller()?.acting_principal().id;
        if current.policy.owner != actor {
            return Err(AgentApiError::forbidden());
        }
        let grants = grants_from_input(current.policy.owner, &access.grants)?;
        store
            .put_policy(
                self.universe_id(),
                actor,
                resource,
                &store_pg::PolicyReplacement {
                    visibility: access.visibility.unwrap_or(Visibility::Universe),
                    grants,
                    expected_revision: None,
                },
                now_ms()? as u64,
            )
            .await
            .map_err(map_policy_error)?;
        Ok(())
    }
}
