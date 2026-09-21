//! Identity persistence acceptance against an isolated schema. Never loads .env.
use std::{panic::AssertUnwindSafe, str::FromStr};

use access::*;
use futures_util::FutureExt as _;
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgAccessStore, PgStore};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly approved LIGHTSPEED_TEST_POSTGRES_URL; creates an isolated schema"]
async fn identity_lifecycle_is_transactional_scoped_and_revocable() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("lightspeed_access_test_{}", Uuid::new_v4().simple());
    admin
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    let outcome = AssertUnwindSafe(exercise(&pool)).catch_unwind().await;
    pool.close().await;
    admin
        .execute(format!("DROP SCHEMA \"{schema}\" CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn role(scope: AccessScope, subject: Subject, role: Role) -> RoleAssignment {
    RoleAssignment {
        scope,
        subject,
        role,
    }
}

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.unwrap();
    PgStore::migrate(pool).await.unwrap();
    let store = PgAccessStore::new(pool.clone());
    let first = Uuid::new_v4();
    let competing = Uuid::new_v4();
    let (a, b) = tokio::join!(
        store.bootstrap(first, "First".into(), 1),
        store.bootstrap(competing, "Other".into(), 1)
    );
    let actor = match (a, b) {
        (Ok(result), Err(AccessError::AlreadyBootstrapped)) => {
            assert_eq!(result.policy_revision, 1);
            first
        }
        (Err(AccessError::AlreadyBootstrapped), Ok(result)) => {
            assert_eq!(result.policy_revision, 1);
            competing
        }
        unexpected => panic!("bootstrap did not have exactly one winner: {unexpected:?}"),
    };
    assert!(
        !store
            .bootstrap(actor, "Retry".into(), 2)
            .await
            .unwrap()
            .changed
    );
    assert!(store.accessible_universes(actor).await.unwrap().is_empty());

    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let service = Uuid::new_v4();
    let u = Uuid::new_v4();
    let v = Uuid::new_v4();
    let scope = AccessScope::Universe { universe_id: u };
    for id in [u, v] {
        assert!(
            store
                .apply(
                    actor,
                    AccessChange::CreateUniverse {
                        universe_id: id,
                        slug: None
                    },
                    3
                )
                .await
                .unwrap()
                .changed
        );
    }
    assert!(
        store
            .effective_access(actor, scope)
            .await
            .unwrap()
            .has_role(Role::Admin)
    );
    for (id, kind, management_scope) in [
        (alice, PrincipalKind::User, AccessScope::Deployment),
        (bob, PrincipalKind::User, AccessScope::Deployment),
        (service, PrincipalKind::Service, scope),
    ] {
        store
            .apply(
                actor,
                AccessChange::CreatePrincipal {
                    id,
                    kind,
                    display_name: id.to_string(),
                    management_scope,
                },
                4,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .apply(
                bob,
                AccessChange::CreateUniverse {
                    universe_id: Uuid::new_v4(),
                    slug: None
                },
                5
            )
            .await,
        Err(AccessError::Denied)
    );
    let retry = store
        .apply(
            actor,
            AccessChange::CreateUniverse {
                universe_id: u,
                slug: None,
            },
            5,
        )
        .await
        .unwrap();
    assert!(!retry.changed);

    let group = Uuid::new_v4();
    store
        .apply(
            actor,
            AccessChange::CreateGroup {
                id: group,
                display_name: "Team".into(),
            },
            6,
        )
        .await
        .unwrap();
    let membership = Membership {
        group_id: group,
        principal_id: alice,
    };
    store
        .apply(actor, AccessChange::PutMembership { membership }, 7)
        .await
        .unwrap();
    let group_role = role(scope, Subject::Group(group), Role::Admin);
    store
        .apply(
            actor,
            AccessChange::AssignRole {
                assignment: group_role,
            },
            8,
        )
        .await
        .unwrap();
    assert_eq!(store.accessible_universes(alice).await.unwrap(), [u]);
    assert!(
        !store
            .effective_access(alice, AccessScope::Deployment)
            .await
            .unwrap()
            .has_role(Role::DeploymentAdmin)
    );
    store
        .apply(
            actor,
            AccessChange::RevokeRole {
                assignment: role(scope, Subject::Principal(actor), Role::Admin),
            },
            9,
        )
        .await
        .unwrap();
    let before = store
        .effective_access(actor, scope)
        .await
        .unwrap()
        .policy_revision;
    assert_eq!(
        store
            .apply(actor, AccessChange::RemoveMembership { membership }, 10)
            .await,
        Err(AccessError::LastAdministrator { scope })
    );
    assert_eq!(
        store
            .effective_access(actor, scope)
            .await
            .unwrap()
            .policy_revision,
        before
    );
    assert!(
        store
            .effective_access(alice, scope)
            .await
            .unwrap()
            .has_role(Role::Admin)
    );

    // Universe administration can assign universe roles, but never a deployment
    // role or service capabilities. Service kind alone grants nothing.
    let bob_role = role(scope, Subject::Principal(bob), Role::Contributor);
    store
        .apply(
            alice,
            AccessChange::AssignRole {
                assignment: bob_role,
            },
            11,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .apply(
                alice,
                AccessChange::AssignRole {
                    assignment: role(
                        AccessScope::Deployment,
                        Subject::Principal(alice),
                        Role::DeploymentAdmin
                    )
                },
                12
            )
            .await,
        Err(AccessError::Denied)
    );
    let capability = CapabilityAssignment {
        scope,
        principal_id: service,
        capability: ServiceCapability::LeaseCredentials,
    };
    assert!(
        !store
            .effective_access(service, scope)
            .await
            .unwrap()
            .has_capability(ServiceCapability::LeaseCredentials)
    );
    assert_eq!(
        store
            .apply(
                alice,
                AccessChange::AssignCapability {
                    assignment: capability
                },
                13
            )
            .await,
        Err(AccessError::Denied)
    );
    store
        .apply(
            actor,
            AccessChange::AssignCapability {
                assignment: capability,
            },
            14,
        )
        .await
        .unwrap();
    assert!(
        store
            .effective_access(service, scope)
            .await
            .unwrap()
            .has_capability(ServiceCapability::LeaseCredentials)
    );
    assert!(
        !store
            .effective_access(service, AccessScope::Universe { universe_id: v })
            .await
            .unwrap()
            .has_capability(ServiceCapability::LeaseCredentials)
    );
    assert!(matches!(
        store
            .apply(
                actor,
                AccessChange::AssignCapability {
                    assignment: CapabilityAssignment {
                        principal_id: alice,
                        ..capability
                    }
                },
                15
            )
            .await,
        Err(AccessError::Invalid(_))
    ));
    assert!(matches!(
        store
            .apply(
                actor,
                AccessChange::AssignCapability {
                    assignment: CapabilityAssignment {
                        scope: AccessScope::Deployment,
                        principal_id: service,
                        capability: ServiceCapability::AssertUser
                    }
                },
                15
            )
            .await,
        Err(AccessError::Invalid(_))
    ));
    store
        .apply(
            actor,
            AccessChange::RevokeCapability {
                assignment: capability,
            },
            16,
        )
        .await
        .unwrap();
    assert!(
        !store
            .effective_access(service, scope)
            .await
            .unwrap()
            .has_capability(ServiceCapability::LeaseCredentials)
    );

    // Disablement may orphan a universe. Recovery is deployment-only, audited,
    // and never gives the recovering administrator content membership.
    store
        .apply(
            actor,
            AccessChange::SetPrincipalStatus {
                id: alice,
                status: PrincipalStatus::Disabled,
            },
            17,
        )
        .await
        .unwrap();
    assert!(store.accessible_universes(alice).await.unwrap().is_empty());
    assert!(
        store
            .effective_access(alice, scope)
            .await
            .unwrap()
            .roles
            .is_empty()
    );
    assert_eq!(
        store
            .apply(
                bob,
                AccessChange::RecoverUniverse {
                    universe_id: u,
                    principal_id: bob
                },
                18
            )
            .await,
        Err(AccessError::Denied)
    );
    store
        .apply(
            actor,
            AccessChange::RecoverUniverse {
                universe_id: u,
                principal_id: bob,
            },
            19,
        )
        .await
        .unwrap();
    assert!(
        store
            .effective_access(actor, scope)
            .await
            .unwrap()
            .roles
            .is_empty()
    );
    assert_eq!(
        store
            .apply(
                actor,
                AccessChange::RecoverUniverse {
                    universe_id: u,
                    principal_id: bob
                },
                20
            )
            .await,
        Err(AccessError::Conflict)
    );
    store
        .apply(
            actor,
            AccessChange::SetPrincipalStatus {
                id: alice,
                status: PrincipalStatus::Active,
            },
            21,
        )
        .await
        .unwrap();

    // Concurrent removal of two independent admin paths cannot remove both.
    let bob_admin = role(scope, Subject::Principal(bob), Role::Admin);
    let (a, b) = tokio::join!(
        store.apply(actor, AccessChange::RemoveMembership { membership }, 22),
        store.apply(
            actor,
            AccessChange::RevokeRole {
                assignment: bob_admin
            },
            22
        )
    );
    assert!(
        matches!(
            (&a, &b),
            (Ok(_), Err(AccessError::LastAdministrator { .. }))
                | (Err(AccessError::LastAdministrator { .. }), Ok(_))
        ),
        "{a:?}, {b:?}"
    );

    // Failed requests roll back their identity, revision, and audit mutations.
    let bad_universe = Uuid::new_v4();
    let invalid_principal = Uuid::new_v4();
    assert_eq!(
        store
            .apply(
                actor,
                AccessChange::CreatePrincipal {
                    id: invalid_principal,
                    kind: PrincipalKind::Service,
                    display_name: "Missing scope".into(),
                    management_scope: AccessScope::Universe {
                        universe_id: bad_universe
                    }
                },
                23
            )
            .await,
        Err(AccessError::NotFound)
    );
    assert!(store.principal(invalid_principal).await.unwrap().is_none());
    let revision = store
        .effective_access(actor, AccessScope::Deployment)
        .await
        .unwrap()
        .policy_revision;
    let audit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_changes")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(audit_count as u64, revision);
    let recovery_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM access_audit_changes WHERE event->>'operation' = 'recover_universe'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(recovery_count, 1);

    // Universe deletion does not delete canonical identities or access audit;
    // their management provenance remains, while scoped authority disappears.
    store_pg::delete_universe(pool, u).await.unwrap();
    assert!(store.principal(service).await.unwrap().is_some());
    assert!(
        store
            .effective_access(service, scope)
            .await
            .unwrap()
            .capabilities
            .is_empty()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM access_audit_changes")
            .fetch_one(pool)
            .await
            .unwrap(),
        audit_count
    );

    // Offboarding the last deployment admin is possible; bootstrap never becomes
    // a backdoor for reactivation or replacement after initialization.
    store
        .apply(
            actor,
            AccessChange::SetPrincipalStatus {
                id: actor,
                status: PrincipalStatus::Disabled,
            },
            24,
        )
        .await
        .unwrap();
    assert_eq!(
        store.bootstrap(actor, "Retry".into(), 25).await,
        Err(AccessError::AlreadyBootstrapped)
    );
    assert_eq!(
        store
            .bootstrap(Uuid::new_v4(), "Replacement".into(), 25)
            .await,
        Err(AccessError::AlreadyBootstrapped)
    );
}
