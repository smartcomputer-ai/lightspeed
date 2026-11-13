//! Authentication of API keys, their scope and groups, and asserted actors.
//!
//! A request is resolved once at the transport boundary into what it
//! addresses, the key that authenticated it, and the actor that key asserted.
//! Handlers never re-resolve it.
//!
//! It happens in two steps. [`resolve_caller`] needs only the request
//! headers and runs before the body is read; [`authorize`] needs the method
//! the body names and decides what that caller may do.
use super::request_context::{KeyContext, RequestContext};
use api::{AccessScope, AgentApiError, MethodGroup, MethodScope};
use auth::{ApiKeyRecord, api_key_hash};
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

/// The longest method name a log line repeats. The name is caller-supplied
/// and a request body may be tens of megabytes, so a refusal never writes
/// more of it than any real method needs.
const MAX_LOGGED_METHOD_CHARS: usize = 128;

/// The caller-supplied method name as a log line may repeat it.
pub fn logged_method(method: &str) -> &str {
    method
        .char_indices()
        .nth(MAX_LOGGED_METHOD_CHARS)
        .map_or(method, |(end, _)| &method[..end])
}

/// Who is calling, resolved from the request headers alone: the key that
/// authenticated the request and the identity claims sent with it. The
/// transport holds one before it reads a request body, so only a caller with
/// a valid key can make the server buffer and parse one. What the caller may
/// do with it is decided per method by [`authorize`].
#[derive(Clone, Debug)]
pub struct Caller {
    key: ApiKeyRecord,
    universe: SelectedUniverse,
    /// The actor header as sent; whether this key may assert it is an
    /// authorization decision.
    actor: Option<String>,
    removed_principal_header: bool,
}

/// What the universe header named.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectedUniverse {
    None,
    Universe(Uuid),
    /// A deployment key named a handle that no universe has.
    UnknownHandle(String),
    /// A universe key sent something other than a UUID. Only a deployment
    /// key may name a universe, so its handle is never looked up.
    HandleFromUniverseKey,
}

/// The API key secret a request presents, if it presents exactly one
/// well-formed one.
fn bearer_secret(headers: &HeaderMap) -> Result<&str, AgentApiError> {
    header_value(headers, header::AUTHORIZATION.as_str())?
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|s| s.starts_with("lsk_") && !s.chars().any(char::is_whitespace))
        .ok_or_else(AgentApiError::unauthenticated)
}

/// Resolve the caller from the request headers. No method is involved, so
/// this runs before the request body is read.
///
/// Fails as `unauthenticated` when the request presents no valid key or a
/// malformed identity header, and as `internal` when the key store cannot
/// answer. Every refusal that depends on what the key may do is left to
/// [`authorize`], which reports it against the request that was sent.
pub async fn resolve_caller(
    keys: &PgApiKeyStore,
    headers: &HeaderMap,
    now_ms: u64,
) -> Result<Caller, AgentApiError> {
    // Everything the headers alone can refuse is refused before the store is
    // asked anything.
    let secret = bearer_secret(headers)?;
    let universe = header_value(headers, UNIVERSE_HEADER)?;
    let actor = header_value(headers, ACTOR_HEADER)?;
    let key = keys
        .resolve_api_key(&api_key_hash(secret), now_ms)
        .await
        .map_err(|error| AgentApiError::internal(error.to_string()))?
        .ok_or_else(AgentApiError::unauthenticated)?;

    // The header names a universe by UUID or by handle. Only a deployment
    // key may name one, so only it pays for the handle lookup, and it is told
    // when the handle names nothing.
    let universe = match universe {
        None => SelectedUniverse::None,
        Some(reference) => match Uuid::parse_str(reference) {
            Ok(universe_id) => SelectedUniverse::Universe(universe_id),
            Err(_) if key.scope == AccessScope::Deployment => keys
                .resolve_universe_handle(reference)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?
                .map_or_else(
                    || SelectedUniverse::UnknownHandle(reference.to_owned()),
                    SelectedUniverse::Universe,
                ),
            Err(_) => SelectedUniverse::HandleFromUniverseKey,
        },
    };
    Ok(Caller {
        key,
        universe,
        actor: actor.map(str::to_owned),
        removed_principal_header: headers.contains_key(REMOVED_PRINCIPAL_HEADER),
    })
}

/// Decide whether an authenticated caller may call `method`, and what the
/// request addresses: the method must exist, the key's scope and the named
/// universe must fit it, the key must be allowed to assert the actor it
/// named, and the key must hold the method's group.
pub fn authorize(caller: Caller, method: &str) -> Result<RequestContext, AgentApiError> {
    let scope = api::method_access(method)
        .ok_or_else(unknown_method)?
        .scope();
    let Caller {
        key,
        universe,
        actor,
        removed_principal_header,
    } = caller;

    // The caller is authenticated; a refusal names its key by the id that
    // outlives the key's secret.
    let forbidden = || {
        tracing::warn!(
            target: "temporal_runtime",
            method = logged_method(method),
            key_id = %key.key_id,
            key_scope = key.scope.kind().as_str(),
            "authenticated request refused"
        );
        AgentApiError::forbidden()
    };

    if removed_principal_header {
        return Err(forbidden());
    }
    let selected = match universe {
        SelectedUniverse::None => None,
        SelectedUniverse::Universe(universe_id) => Some(universe_id),
        SelectedUniverse::UnknownHandle(reference) => {
            return Err(AgentApiError::not_found(format!(
                "unknown universe: {reference}"
            )));
        }
        SelectedUniverse::HandleFromUniverseKey => return Err(forbidden()),
    };
    let target = target_scope(scope, key.scope, selected).ok_or_else(forbidden)?;
    let actor = match actor {
        None => None,
        Some(actor) if key.assert_actor && valid_actor(&actor) => Some(actor),
        Some(_) => return Err(forbidden()),
    };
    if MethodGroup::of(method).is_some_and(|group| !key.groups.contains(&group)) {
        return Err(forbidden());
    }
    Ok(RequestContext {
        scope: target,
        key: Some(KeyContext {
            scope: key.scope,
            key_id: key.key_id,
            prefix: key.key_prefix,
            groups: key.groups,
        }),
        actor,
    })
}

/// What a request addresses. A universe key reaches only its universe and
/// takes no universe header; a deployment key names the universe of a
/// universe method by header, as its UUID or its handle; deployment methods need a deployment key and
/// take no universe.
fn target_scope(
    method: MethodScope,
    key: AccessScope,
    selected: Option<Uuid>,
) -> Option<AccessScope> {
    match (method, key, selected) {
        (MethodScope::Connection, _, None) => Some(key),
        (MethodScope::Connection, _, Some(_)) => None,
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
    use api::{AgentApiErrorKind, Attribution};

    fn universe(id: u128) -> AccessScope {
        AccessScope::Universe {
            universe_id: Uuid::from_u128(id),
        }
    }

    const SECRET: &str = "lsk_0123456789abcdef0123456789abcdef";
    const AUTHORIZATION: &str = "authorization";
    const BEARER: &str = "Bearer lsk_0123456789abcdef0123456789abcdef";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    /// A key store whose database is never reachable: a refusal that still
    /// comes back as `unauthenticated` was decided from the headers alone.
    fn unreachable_keys() -> PgApiKeyStore {
        PgApiKeyStore::new(
            sqlx::postgres::PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(200))
                .connect_lazy("postgres://127.0.0.1:1/unreachable")
                .unwrap(),
        )
    }

    fn key(scope: AccessScope, groups: &[MethodGroup], assert_actor: bool) -> ApiKeyRecord {
        ApiKeyRecord {
            key_id: "key_test".into(),
            key_prefix: "lsk_01234567".into(),
            scope,
            groups: groups.iter().copied().collect(),
            assert_actor,
            created_by: Attribution::Local,
            display_name: None,
            created_at_ms: 1,
            revoked_at_ms: None,
            last_used_at_ms: None,
        }
    }

    fn caller(key: ApiKeyRecord) -> Caller {
        Caller {
            key,
            universe: SelectedUniverse::None,
            actor: None,
            removed_principal_header: false,
        }
    }

    fn kind(result: Result<RequestContext, AgentApiError>) -> AgentApiErrorKind {
        result.expect_err("the request is refused").kind
    }

    #[test]
    fn a_logged_method_is_bounded_on_a_character_boundary() {
        assert_eq!(logged_method("session/read"), "session/read");
        let exact = "m".repeat(MAX_LOGGED_METHOD_CHARS);
        assert_eq!(logged_method(&exact), exact);
        let long = "é".repeat(MAX_LOGGED_METHOD_CHARS + 50);
        let logged = logged_method(&long);
        assert_eq!(logged.chars().count(), MAX_LOGGED_METHOD_CHARS);
        assert!(long.starts_with(logged));
    }

    #[test]
    fn only_one_well_formed_bearer_key_is_a_presented_secret() {
        assert_eq!(
            bearer_secret(&headers(&[(AUTHORIZATION, BEARER)])).unwrap(),
            SECRET
        );
        for refused in [
            headers(&[]),
            headers(&[(AUTHORIZATION, SECRET)]),
            headers(&[(AUTHORIZATION, "Basic bHNrXzAxMjM6")]),
            headers(&[(AUTHORIZATION, "Bearer not-a-lightspeed-key")]),
            headers(&[(AUTHORIZATION, "Bearer lsk_0123 4567")]),
            headers(&[(AUTHORIZATION, BEARER), (AUTHORIZATION, BEARER)]),
        ] {
            assert_eq!(
                bearer_secret(&refused).unwrap_err().kind,
                AgentApiErrorKind::Unauthenticated,
                "{refused:?}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_caller_without_a_presentable_key_is_refused_without_the_key_store() {
        let keys = unreachable_keys();
        for refused in [
            headers(&[]),
            headers(&[(AUTHORIZATION, "Bearer not-a-lightspeed-key")]),
            headers(&[(AUTHORIZATION, BEARER), (AUTHORIZATION, BEARER)]),
            // A repeated identity header is refused before the key is looked
            // up: a proxy and the gateway could disagree about which one counts.
            headers(&[
                (AUTHORIZATION, BEARER),
                (UNIVERSE_HEADER, "one"),
                (UNIVERSE_HEADER, "two"),
            ]),
            headers(&[
                (AUTHORIZATION, BEARER),
                (ACTOR_HEADER, "a"),
                (ACTOR_HEADER, "b"),
            ]),
        ] {
            let error = resolve_caller(&keys, &refused, 1)
                .await
                .expect_err("the caller is refused");
            assert_eq!(
                error.kind,
                AgentApiErrorKind::Unauthenticated,
                "{refused:?}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_key_store_failure_is_not_reported_as_a_bad_key() {
        let presented = headers(&[(AUTHORIZATION, BEARER)]);
        let error = resolve_caller(&unreachable_keys(), &presented, 1)
            .await
            .expect_err("the key cannot be resolved");
        assert_eq!(error.kind, AgentApiErrorKind::Internal);
    }

    #[test]
    fn an_authenticated_caller_is_told_when_the_method_does_not_exist() {
        let deployment = key(AccessScope::Deployment, &[MethodGroup::Session], false);
        assert_eq!(
            kind(authorize(caller(deployment), "no/such/method")),
            AgentApiErrorKind::InvalidRequest
        );
    }

    #[test]
    fn a_key_reaches_what_its_scope_and_groups_allow() {
        let universe_key = key(universe(1), &[MethodGroup::Session], false);
        let context = authorize(caller(universe_key.clone()), "session/read").unwrap();
        assert_eq!(context.scope, universe(1));
        assert_eq!(context.actor, None);
        let held = context.key.expect("a keyed request");
        assert_eq!(held.scope, universe(1));
        assert_eq!(held.key_id, "key_test");
        assert_eq!(held.prefix, "lsk_01234567");
        assert_eq!(held.groups, [MethodGroup::Session].into());

        // `initialize` belongs to no group: any key may call it.
        let no_groups = key(universe(1), &[], false);
        assert_eq!(
            authorize(caller(no_groups), api::METHOD_INITIALIZE)
                .unwrap()
                .scope,
            universe(1)
        );

        // A group the key does not hold, and a deployment method.
        assert_eq!(
            kind(authorize(caller(universe_key.clone()), "profiles/list")),
            AgentApiErrorKind::Forbidden
        );
        assert_eq!(
            kind(authorize(
                caller(key(universe(1), &[MethodGroup::DeploymentApiKeys], false)),
                "deployment/api-keys/list"
            )),
            AgentApiErrorKind::Forbidden
        );
    }

    /// Attribution names the key, not what the request addressed: a
    /// deployment key working in a universe is still a deployment key, and
    /// replacing a key's secret does not change who its requests are from.
    #[test]
    fn a_keyed_request_is_attributed_to_the_key_id_and_the_keys_own_scope() {
        let in_universe = |key| Caller {
            universe: SelectedUniverse::Universe(Uuid::from_u128(2)),
            ..caller(key)
        };
        let deployment = key(AccessScope::Deployment, &[MethodGroup::Session], false);
        let context = authorize(in_universe(deployment.clone()), "session/read").unwrap();
        assert_eq!(context.scope, universe(2));
        assert_eq!(
            context.attribution(),
            Attribution::Key {
                key_id: "key_test".into(),
                scope_kind: api::AccessScopeKind::Deployment,
            }
        );
        let rotated = ApiKeyRecord {
            key_prefix: "lsk_89abcdef".into(),
            ..deployment
        };
        assert_eq!(
            authorize(in_universe(rotated), "session/read")
                .unwrap()
                .attribution(),
            context.attribution()
        );

        let universe_key = key(universe(1), &[MethodGroup::Session], false);
        assert_eq!(
            authorize(caller(universe_key), "session/read")
                .unwrap()
                .attribution(),
            Attribution::Key {
                key_id: "key_test".into(),
                scope_kind: api::AccessScopeKind::Universe,
            }
        );
    }

    #[test]
    fn the_universe_header_is_judged_against_the_key_scope() {
        let deployment = || key(AccessScope::Deployment, &[MethodGroup::Session], false);
        let naming = |key, universe| Caller {
            universe,
            ..caller(key)
        };
        let selected = SelectedUniverse::Universe(Uuid::from_u128(2));
        assert_eq!(
            authorize(naming(deployment(), selected.clone()), "session/read")
                .unwrap()
                .scope,
            universe(2)
        );
        // A deployment key must name the universe of a universe method.
        assert_eq!(
            kind(authorize(caller(deployment()), "session/read")),
            AgentApiErrorKind::Forbidden
        );
        // It is told when a handle names nothing.
        let unknown = authorize(
            naming(
                deployment(),
                SelectedUniverse::UnknownHandle("nowhere".into()),
            ),
            "session/read",
        )
        .expect_err("the handle names nothing");
        assert_eq!(unknown.kind, AgentApiErrorKind::NotFound);
        // A universe key names no universe, by UUID or by handle.
        let universe_key = || key(universe(1), &[MethodGroup::Session], false);
        for named in [selected, SelectedUniverse::HandleFromUniverseKey] {
            assert_eq!(
                kind(authorize(naming(universe_key(), named), "session/read")),
                AgentApiErrorKind::Forbidden
            );
        }
    }

    #[test]
    fn only_a_key_allowed_to_assert_an_actor_may_name_one() {
        let acting = |assert_actor, actor: &str| Caller {
            actor: Some(actor.to_owned()),
            ..caller(key(universe(1), &[MethodGroup::Session], assert_actor))
        };
        assert_eq!(
            authorize(acting(true, "user-1"), "session/read")
                .unwrap()
                .actor
                .as_deref(),
            Some("user-1")
        );
        assert_eq!(
            kind(authorize(acting(false, "user-1"), "session/read")),
            AgentApiErrorKind::Forbidden
        );
        for malformed in [String::new(), "x".repeat(257), "line\nbreak".to_owned()] {
            assert_eq!(
                kind(authorize(acting(true, &malformed), "session/read")),
                AgentApiErrorKind::Forbidden,
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn the_removed_principal_header_is_refused_for_an_authenticated_caller() {
        let refused = Caller {
            removed_principal_header: true,
            ..caller(key(universe(1), &[MethodGroup::Session], false))
        };
        assert_eq!(
            kind(authorize(refused.clone(), "session/read")),
            AgentApiErrorKind::Forbidden
        );
        // The method is checked first, as it is for every caller.
        assert_eq!(
            kind(authorize(refused, "no/such/method")),
            AgentApiErrorKind::InvalidRequest
        );
    }

    #[test]
    fn connection_discovery_needs_no_selected_universe_and_never_accepts_one() {
        for key in [AccessScope::Deployment, universe(1)] {
            assert_eq!(target_scope(MethodScope::Connection, key, None), Some(key));
            assert_eq!(
                target_scope(MethodScope::Connection, key, Some(Uuid::from_u128(1))),
                None
            );
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
