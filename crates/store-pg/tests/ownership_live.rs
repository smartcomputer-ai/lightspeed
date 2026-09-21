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
async fn ownership_is_immutable_and_controller_lineage_is_explicit() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("lightspeed_ownership_test_{}", Uuid::new_v4().simple());
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

async fn exercise(pool: &sqlx::PgPool) {
    PgStore::migrate(pool).await.unwrap();
    let store = PgAccessStore::new(pool.clone());
    let admin = Uuid::new_v4();
    store
        .bootstrap(admin, "Administrator".into(), 1)
        .await
        .unwrap();
    let universe = Uuid::new_v4();
    store
        .apply(
            admin,
            AccessChange::CreateUniverse {
                universe_id: universe,
                slug: None,
            },
            2,
        )
        .await
        .unwrap();
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let mut users = Vec::new();
    for role in [
        Role::Viewer,
        Role::Contributor,
        Role::Contributor,
        Role::Operator,
        Role::Admin,
    ] {
        let id = Uuid::new_v4();
        store
            .apply(
                admin,
                AccessChange::CreatePrincipal {
                    id,
                    kind: PrincipalKind::User,
                    display_name: format!("{role:?}"),
                    management_scope: AccessScope::Deployment,
                },
                3,
            )
            .await
            .unwrap();
        store
            .apply(
                admin,
                AccessChange::AssignRole {
                    assignment: RoleAssignment {
                        scope,
                        subject: Subject::Principal(id),
                        role,
                    },
                },
                4,
            )
            .await
            .unwrap();
        users.push(store.effective_access(id, scope).await.unwrap());
    }
    let [viewer, alice, bob, operator, universe_admin] = users.as_slice() else {
        unreachable!()
    };
    let principal = ResourceController::Principal(alice.principal.id);
    let own = |resource, controller| ResourceOwnership {
        resource,
        created_by: ActionActor::Principal {
            id: alice.principal.id,
        },
        controller,
        created_at_ms: 5,
    };
    let personal = ResourceRef::Session("personal".into());
    let bot = ResourceRef::Bot("assistant".into());
    let bot_session = ResourceRef::Session("bot-session".into());
    let child = ResourceRef::Session("child".into());
    let bot_child = ResourceRef::Session("bot-child".into());
    let profile = ResourceRef::Profile("template".into());
    for record in [
        own(personal.clone(), principal.clone()),
        own(bot.clone(), principal.clone()),
        own(
            bot_session.clone(),
            ResourceController::Bot("assistant".into()),
        ),
        own(
            child.clone(),
            ResourceController::Session("personal".into()),
        ),
        own(
            bot_child.clone(),
            ResourceController::Session("bot-session".into()),
        ),
        own(profile.clone(), principal.clone()),
    ] {
        store.reserve_ownership(universe, &record).await.unwrap();
        store.reserve_ownership(universe, &record).await.unwrap();
    }
    for resource in [&personal, &child] {
        for caller in [alice, bob, operator, universe_admin, viewer] {
            let expected = caller.principal.id == alice.principal.id;
            for action in [
                UniverseAction::ControlSession,
                UniverseAction::DeleteSession,
            ] {
                assert_eq!(
                    store
                        .resource_permitted(caller, action, Some(resource))
                        .await
                        .unwrap(),
                    expected,
                    "{:?} {action:?} {resource:?}",
                    caller.roles
                );
            }
            let stop = expected || caller.has_role(Role::Operator) || caller.has_role(Role::Admin);
            assert_eq!(
                store
                    .resource_permitted(caller, UniverseAction::StopSession, Some(resource))
                    .await
                    .unwrap(),
                stop
            );
        }
    }
    for resource in [&bot_session, &bot_child] {
        for caller in [alice, operator, universe_admin] {
            assert!(
                store
                    .resource_permitted(caller, UniverseAction::ControlSession, Some(resource))
                    .await
                    .unwrap()
            );
        }
        for caller in [bob, viewer] {
            assert!(
                !store
                    .resource_permitted(caller, UniverseAction::ControlSession, Some(resource))
                    .await
                    .unwrap()
            );
        }
    }
    for (resource, action) in [
        (&bot, UniverseAction::ManageBot),
        (&profile, UniverseAction::ManageProfile),
    ] {
        assert!(
            store
                .resource_permitted(alice, action, Some(resource))
                .await
                .unwrap()
        );
        assert!(
            store
                .resource_permitted(operator, action, Some(resource))
                .await
                .unwrap()
        );
        assert!(
            !store
                .resource_permitted(bob, action, Some(resource))
                .await
                .unwrap()
        );
    }
    // Creation races cannot transfer a resource, and the original attribution remains.
    let first = own(ResourceRef::Session("race".into()), principal.clone());
    let mut other = first.clone();
    other.controller = ResourceController::Principal(bob.principal.id);
    other.created_by = ActionActor::Principal {
        id: bob.principal.id,
    };
    let (a, b) = tokio::join!(
        store.reserve_ownership(universe, &first),
        store.reserve_ownership(universe, &other)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()),
        Some(AccessError::Denied)
    ));
    // Scope, missing lineage and cycles all fail closed.
    assert!(
        store
            .ownership(Uuid::new_v4(), &personal)
            .await
            .unwrap()
            .is_none()
    );
    let missing = ResourceRef::Session("missing".into());
    assert!(
        !store
            .resource_permitted(alice, UniverseAction::ControlSession, Some(&missing))
            .await
            .unwrap()
    );
    store
        .reserve_ownership(
            universe,
            &own(
                missing.clone(),
                ResourceController::Session("missing".into()),
            ),
        )
        .await
        .unwrap();
    assert!(
        !store
            .resource_permitted(alice, UniverseAction::ControlSession, Some(&missing))
            .await
            .unwrap()
    );
    // A history fork carries provenance but has its own explicit controller.
    let fork = own(
        ResourceRef::Session("fork".into()),
        ResourceController::Principal(bob.principal.id),
    );
    store.reserve_ownership(universe, &fork).await.unwrap();
    assert!(
        !store
            .resource_permitted(alice, UniverseAction::ControlSession, Some(&fork.resource))
            .await
            .unwrap()
    );
    assert!(
        store
            .resource_permitted(bob, UniverseAction::ControlSession, Some(&fork.resource))
            .await
            .unwrap()
    );

    // Ownership reservations alone must not advertise usable controls. This
    // includes broad operator grants that normally need no ownership lookup.
    for caller in [alice, operator, universe_admin] {
        for resource in [&personal, &bot, &profile] {
            assert!(
                store
                    .resource_actions(caller, resource, false)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
    for id in ["personal", "bot-session", "child", "bot-child", "fork"] {
        sqlx::query("INSERT INTO sessions(universe_id,session_id,retention_root_session_id,created_at_ms,updated_at_ms) VALUES($1,$2,$2,1,1)")
            .bind(universe).bind(id).execute(pool).await.unwrap();
    }
    sqlx::query("UPDATE sessions SET source_session_id='personal',source_seq=0,retention_root_session_id='personal' WHERE universe_id=$1 AND session_id='fork'")
        .bind(universe).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO bots(universe_id,bot_id,revision,document_json,created_at_ms,updated_at_ms) VALUES($1,'assistant',1,'{}',1,1)")
        .bind(universe).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO agent_profiles(universe_id,profile_id,revision,document_json,created_at_ms,updated_at_ms) VALUES($1,'template',1,'{}',1,1)")
        .bind(universe).execute(pool).await.unwrap();

    use UniverseAction::*;
    assert_eq!(
        store
            .resource_actions(viewer, &personal, false)
            .await
            .unwrap(),
        vec![Read]
    );
    assert_eq!(
        store
            .resource_actions(alice, &personal, false)
            .await
            .unwrap(),
        vec![Read, ControlSession, StopSession, DeleteSession]
    );
    for caller in [operator, universe_admin] {
        assert_eq!(
            store
                .resource_actions(caller, &personal, false)
                .await
                .unwrap(),
            vec![Read, StopSession]
        );
        assert_eq!(
            store
                .resource_actions(caller, &bot_child, false)
                .await
                .unwrap(),
            vec![Read, ControlSession, StopSession, DeleteSession]
        );
        assert_eq!(
            store.resource_actions(caller, &bot, false).await.unwrap(),
            vec![Read, ManageBot, InvokeBot]
        );
        assert_eq!(
            store
                .resource_actions(caller, &profile, false)
                .await
                .unwrap(),
            vec![Read, ManageProfile]
        );
    }
    assert_eq!(
        store.resource_actions(bob, &bot, false).await.unwrap(),
        vec![Read, InvokeBot]
    );
    assert_eq!(
        store.resource_actions(bob, &profile, false).await.unwrap(),
        vec![Read]
    );
    // Another principal's history fork blocks cascade deletion, although the
    // parent still permits non-cascade deletion subject to leaf validation.
    assert_eq!(
        store
            .resource_actions(alice, &personal, true)
            .await
            .unwrap(),
        vec![Read, ControlSession, StopSession]
    );
    assert_eq!(
        store
            .resource_actions(bob, &fork.resource, true)
            .await
            .unwrap(),
        vec![Read, ControlSession, StopSession, DeleteSession]
    );
    let mut foreign_scope = alice.clone();
    foreign_scope.scope = AccessScope::Universe {
        universe_id: Uuid::new_v4(),
    };
    assert!(
        store
            .resource_actions(&foreign_scope, &personal, false)
            .await
            .unwrap()
            .is_empty()
    );
    let audit_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM access_audit_events WHERE scope->>'universeId'=$1",
    )
    .bind(universe.to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        audit_events, 0,
        "preview must not record hypothetical admissions"
    );
}
