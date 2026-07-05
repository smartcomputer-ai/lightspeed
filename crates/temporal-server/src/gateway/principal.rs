//! Request-scoped caller principal.
//!
//! The JSON-RPC method surface carries no caller identity (P90: the universe
//! and principal live in the credential, never in request parameters), so the
//! HTTP edge propagates the resolved principal to service methods through a
//! task-local scope around dispatch — the same shape as tracing spans. Service
//! code reads [`request_principal`] where it stamps `PrincipalRef` onto
//! grants and flows; outside a request scope (worker activities, startup,
//! tests) it falls back to `universe_default`, the pre-P90 behavior.

use auth::PrincipalRef;

tokio::task_local! {
    static REQUEST_PRINCIPAL: PrincipalRef;
}

/// Run `future` with `principal` as the current request's caller identity.
pub async fn with_request_principal<F>(principal: PrincipalRef, future: F) -> F::Output
where
    F: Future,
{
    REQUEST_PRINCIPAL.scope(principal, future).await
}

/// The caller principal of the current request, or `universe_default` when
/// not inside a request scope.
pub fn request_principal() -> PrincipalRef {
    REQUEST_PRINCIPAL
        .try_with(Clone::clone)
        .unwrap_or_else(|_| PrincipalRef::universe_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use auth::PrincipalKind;

    #[tokio::test(flavor = "current_thread")]
    async fn request_principal_reads_the_scoped_value_and_defaults_outside() {
        assert_eq!(request_principal(), PrincipalRef::universe_default());
        let scoped = PrincipalRef {
            kind: PrincipalKind::ServiceAccount,
            id: Some("bridge-1".to_owned()),
        };
        let observed = with_request_principal(scoped.clone(), async { request_principal() }).await;
        assert_eq!(observed, scoped);
        assert_eq!(request_principal(), PrincipalRef::universe_default());
    }
}
