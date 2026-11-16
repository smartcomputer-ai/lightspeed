//! Authentication of trusted components and resolution of request targets.
//! Tokens have identical authority. Actor names provide attribution only.
use super::request_context::RequestContext;
use crate::config::RpcTokens;
use api::{AccessScope, AgentApiError, MethodScope};
use axum::http::{HeaderMap, header};
use uuid::Uuid;

pub const UNIVERSE_HEADER: &str = "x-lightspeed-universe";
/// An opaque actor recorded for this request. It conveys no authority.
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
pub(super) fn valid_actor(value: &str) -> bool {
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

/// Resolved once, before the request body is read.
#[derive(Clone, Debug)]
pub struct Caller {
    token_name: String,
    universe: Option<Uuid>,
    actor: Option<String>,
}

fn bearer_secret(headers: &HeaderMap) -> Result<&str, AgentApiError> {
    header_value(headers, header::AUTHORIZATION.as_str())?
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or_else(AgentApiError::unauthenticated)
}

pub async fn resolve_caller(
    tokens: &RpcTokens,
    pool: &sqlx::PgPool,
    headers: &HeaderMap,
) -> Result<Caller, AgentApiError> {
    let secret = bearer_secret(headers)?;
    let token_name = tokens
        .authenticate(secret)
        .ok_or_else(AgentApiError::unauthenticated)?;
    let universe = header_value(headers, UNIVERSE_HEADER)?;
    let actor = header_value(headers, ACTOR_HEADER)?;
    if headers.contains_key(REMOVED_PRINCIPAL_HEADER)
        || actor.is_some_and(|value| !valid_actor(value))
    {
        return Err(AgentApiError::invalid_request("invalid identity header"));
    }
    let universe = match universe {
        None => None,
        Some(reference) => Some(
            store_pg::resolve_universe_reference(pool, reference)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?
                .ok_or_else(|| {
                    AgentApiError::not_found(format!("unknown universe: {reference}"))
                })?,
        ),
    };
    Ok(Caller {
        token_name: token_name.to_owned(),
        universe,
        actor: actor.map(str::to_owned),
    })
}

/// Validate the address for the method, without applying caller permissions.
pub fn request_context(caller: Caller, method: &str) -> Result<RequestContext, AgentApiError> {
    let method_scope = api::method_access(method)
        .ok_or_else(unknown_method)?
        .scope();
    let scope = match (method_scope, caller.universe) {
        (MethodScope::Connection | MethodScope::Deployment, None) => AccessScope::Deployment,
        (MethodScope::Universe | MethodScope::Service, Some(universe_id)) => {
            AccessScope::Universe { universe_id }
        }
        (MethodScope::Connection | MethodScope::Deployment, Some(_)) => {
            return Err(AgentApiError::invalid_request(
                "this method takes no universe header",
            ));
        }
        (_, None) => {
            return Err(AgentApiError::invalid_request(
                "a universe method requires x-lightspeed-universe",
            ));
        }
    };
    Ok(RequestContext {
        scope,
        token_name: Some(caller.token_name),
        actor: caller.actor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{AgentApiErrorKind, Attribution};
    const SECRET: &str = "0123456789abcdef0123456789abcdef";
    fn tokens() -> RpcTokens {
        RpcTokens::from_configuration(
            [("LIGHTSPEED_RPC_TOKEN_PLATFORM".into(), SECRET.into())],
            |_| unreachable!(),
        )
        .unwrap()
    }
    fn pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .unwrap()
    }
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
    #[tokio::test(flavor = "current_thread")]
    async fn token_authentication_needs_no_database_and_rejects_ambiguous_headers() {
        let auth = format!("Bearer {SECRET}");
        let tokens = tokens();
        let pool = pool();
        let caller = resolve_caller(
            &tokens,
            &pool,
            &headers(&[("authorization", &auth), (ACTOR_HEADER, "alice")]),
        )
        .await
        .unwrap();
        let context = request_context(caller, "initialize").unwrap();
        assert_eq!(context.token_name.as_deref(), Some("platform"));
        assert_eq!(
            context.attribution(),
            Attribution::Actor { id: "alice".into() }
        );
        for pairs in [
            vec![],
            vec![("authorization", "Bearer unknown")],
            vec![("authorization", &auth), ("authorization", &auth)],
            vec![
                ("authorization", &auth),
                (ACTOR_HEADER, "alice"),
                (ACTOR_HEADER, "bob"),
            ],
        ] {
            assert_eq!(
                resolve_caller(&tokens, &pool, &headers(&pairs))
                    .await
                    .unwrap_err()
                    .kind,
                AgentApiErrorKind::Unauthenticated
            );
        }
    }
    #[test]
    fn all_tokens_reach_all_methods_and_actors_change_only_attribution() {
        for token_name in ["platform", "operator"] {
            for actor in [None, Some("alice".into()), Some("bob".into())] {
                let make = |universe| Caller {
                    token_name: token_name.into(),
                    universe,
                    actor: actor.clone(),
                };
                let context = request_context(make(Some(Uuid::nil())), "session/read").unwrap();
                assert_eq!(
                    context.scope,
                    AccessScope::Universe {
                        universe_id: Uuid::nil()
                    }
                );
                assert_eq!(
                    context.attribution(),
                    actor.clone().map_or(
                        Attribution::Token {
                            name: token_name.into()
                        },
                        |id| Attribution::Actor { id }
                    )
                );
                assert!(request_context(make(None), "deployment/universes/list").is_ok());
                assert!(request_context(make(Some(Uuid::nil())), "transcriptions/read").is_ok());
                assert!(request_context(make(Some(Uuid::nil())), "transcriptions/cancel").is_ok());
                assert!(request_context(make(None), "session/read").is_err());
                assert!(request_context(make(Some(Uuid::nil())), "initialize").is_err());
                assert!(request_context(make(None), "deployment/api-keys/create").is_err());
            }
        }
    }
    #[test]
    fn a_logged_method_is_bounded_on_a_character_boundary() {
        assert_eq!(logged_method("session/read"), "session/read");
        assert_eq!(
            logged_method(&"é".repeat(200)).chars().count(),
            MAX_LOGGED_METHOD_CHARS
        );
    }
}
