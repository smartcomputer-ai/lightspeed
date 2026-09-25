//! Authentication of API keys, their scope and groups, and asserted actors.
//!
//! A request is resolved once at the transport boundary into what it
//! addresses, the key that authenticated it, and the actor that key asserted.
//! Handlers never re-resolve it.
use super::request_context::{KeyContext, RequestContext};
use api::{AccessScope, AgentApiError, MethodGroup, MethodScope};
use auth::api_key_hash;
use axum::http::{HeaderMap, header};
use store_pg::PgApiKeyStore;
use uuid::Uuid;

pub const UNIVERSE_HEADER: &str = "x-lightspeed-universe";
/// The actor a key allowed to assert one names for its request.
pub const ACTOR_HEADER: &str = "x-lightspeed-actor";
const REMOVED_PRINCIPAL_HEADER: &str = "x-lightspeed-principal";

/// Reject duplicates rather than allowing proxy/framework disagreement.
fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, AgentApiError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(AgentApiError::unauthenticated());
    }
    value
        .map(|v| v.to_str().map_err(|_| AgentApiError::unauthenticated()))
        .transpose()
}

/// An actor is an opaque identifier: non-empty, bounded, and free of control
/// characters so it can be stored and logged verbatim.
fn valid_actor(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

pub fn reject_identity_headers(headers: &HeaderMap) -> Result<(), AgentApiError> {
    for name in [
        header::AUTHORIZATION.as_str(),
        UNIVERSE_HEADER,
        ACTOR_HEADER,
        REMOVED_PRINCIPAL_HEADER,
    ] {
        if headers.contains_key(name) {
            return Err(AgentApiError::invalid_request(
                "identity headers are not accepted in single mode",
            ));
        }
    }
    Ok(())
}

pub fn unknown_method() -> AgentApiError {
    AgentApiError::invalid_request("unknown method")
}

pub async fn authenticate(
    keys: &PgApiKeyStore,
    headers: &HeaderMap,
    method: &str,
    now_ms: u64,
) -> Result<RequestContext, AgentApiError> {
    let scope = api::method_access(method)
        .ok_or_else(unknown_method)?
        .scope();
    let secret = header_value(headers, header::AUTHORIZATION.as_str())?
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|s| s.starts_with("lsk_") && !s.chars().any(char::is_whitespace))
        .ok_or_else(AgentApiError::unauthenticated)?;
    let key = keys
        .resolve_api_key(&api_key_hash(secret), now_ms)
        .await
        .map_err(|error| AgentApiError::internal(error.to_string()))?
        .ok_or_else(AgentApiError::unauthenticated)?;

    // The caller is authenticated from here on; a refusal names its key.
    let forbidden = || {
        tracing::warn!(
            target: "temporal_server",
            %method,
            key = %key.key_prefix,
            "authenticated request refused"
        );
        AgentApiError::forbidden()
    };

    if headers.contains_key(REMOVED_PRINCIPAL_HEADER) {
        return Err(forbidden());
    }

    let selected = match header_value(headers, UNIVERSE_HEADER)?.map(Uuid::parse_str) {
        Some(Ok(universe_id)) => Some(universe_id),
        Some(Err(_)) => return Err(forbidden()),
        None => None,
    };
    let target = target_scope(scope, key.scope, selected).ok_or_else(forbidden)?;
    let actor = match header_value(headers, ACTOR_HEADER)? {
        None => None,
        Some(actor) if key.assert_actor && valid_actor(actor) => Some(actor.to_owned()),
        Some(_) => return Err(forbidden()),
    };
    if MethodGroup::of(method).is_some_and(|group| !key.groups.contains(&group)) {
        return Err(forbidden());
    }
    Ok(RequestContext {
        scope: target,
        key: Some(KeyContext {
            prefix: key.key_prefix,
            groups: key.groups,
        }),
        actor,
    })
}

/// What a request addresses. A universe key reaches only its universe and
/// takes no universe header; a deployment key names the universe of a
/// universe method by header; deployment methods need a deployment key and
/// take no universe.
fn target_scope(
    method: MethodScope,
    key: AccessScope,
    selected: Option<Uuid>,
) -> Option<AccessScope> {
    match (method, key, selected) {
        (MethodScope::Deployment, AccessScope::Deployment, None) => Some(AccessScope::Deployment),
        (MethodScope::Deployment, _, _) => None,
        (_, AccessScope::Universe { universe_id }, None) => {
            Some(AccessScope::Universe { universe_id })
        }
        (_, AccessScope::Universe { .. }, Some(_)) => None,
        (_, AccessScope::Deployment, Some(universe_id)) => {
            Some(AccessScope::Universe { universe_id })
        }
        (_, AccessScope::Deployment, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn universe(id: u128) -> AccessScope {
        AccessScope::Universe {
            universe_id: Uuid::from_u128(id),
        }
    }

    #[test]
    fn a_key_reaches_its_universe_or_names_one_from_the_deployment() {
        for method in [MethodScope::Universe, MethodScope::Service] {
            assert_eq!(target_scope(method, universe(1), None), Some(universe(1)));
            // A universe key takes no header, not even its own universe.
            assert_eq!(
                target_scope(method, universe(1), Some(Uuid::from_u128(1))),
                None
            );
            assert_eq!(target_scope(method, AccessScope::Deployment, None), None);
            assert_eq!(
                target_scope(method, AccessScope::Deployment, Some(Uuid::from_u128(2))),
                Some(universe(2))
            );
        }
        let deployment = MethodScope::Deployment;
        assert_eq!(
            target_scope(deployment, AccessScope::Deployment, None),
            Some(AccessScope::Deployment)
        );
        assert_eq!(
            target_scope(
                deployment,
                AccessScope::Deployment,
                Some(Uuid::from_u128(2))
            ),
            None
        );
        assert_eq!(target_scope(deployment, universe(1), None), None);
    }
}
