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
async fn anchors_are_immutable_and_decisions_follow_root_policy() {
    let url =
        std::env::var("LIGHTSPEED_TEST_POSTGRES_URL").expect("explicit test Postgres URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("lightspeed_resources_test_{}", Uuid::new_v4().simple());
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
    let alice_actor = ActionActor::Principal {
        id: alice.principal.id,
    };
    let own = |resource: ResourceRef, controller: ResourceController| (resource, controller);
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
        // Retries are idempotent and return the original facts.
        for _ in 0..2 {
            let stored = store
                .reserve_resource(universe, &record.0, &alice_actor, &record.1, None, 5)
                .await
                .unwrap();
            assert_eq!(stored.created_by, alice_actor);
        }
    }
    // The audience root and managing bot are copied down the admitted
    // lineage; only roots carry a policy, owned by their creator and
    // universe-visible.
    for (resource, expected_root, expected_bot) in [
        (&personal, &personal, None),
        (&child, &personal, None),
        (&bot, &bot, None),
        (&bot_session, &bot, Some("assistant")),
        (&bot_child, &bot, Some("assistant")),
        (&profile, &profile, None),
    ] {
        let access = store
            .resource_access(universe, Some(alice.principal.id), resource)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&access.anchor.audience_root, expected_root, "{resource:?}");
        assert_eq!(access.anchor.bot.as_deref(), expected_bot, "{resource:?}");
        let policy = access
            .policy
            .expect("root policy resolved through the root");
        assert_eq!(policy.owner, alice.principal.id, "{resource:?}");
        assert_eq!(policy.visibility, Visibility::Universe);
        assert_eq!(access.grant, None);
    }
    let policy_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM access_resource_policies WHERE universe_id=$1")
            .bind(universe)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        policy_rows, 3,
        "personal, bot and profile are the only roots"
    );
    let permitted = |caller: &EffectiveAccess, action, resource: &ResourceRef| {
        let caller = caller.clone();
        let resource = resource.clone();
        let store = store.clone();
        async move {
            store
                .decide(Caller::Request(&caller), action, &resource)
                .await
                .unwrap()
                == Some(Decision::Allowed)
        }
    };
    for resource in [&personal, &child] {
        for caller in [alice, bob, operator, universe_admin, viewer] {
            let expected = caller.principal.id == alice.principal.id;
            assert_eq!(
                permitted(caller, UniverseAction::ControlSession, resource).await,
                expected,
                "{:?} {resource:?}",
                caller.roles
            );
            // Admin deletes anyone's session; nobody else deletes another's.
            assert_eq!(
                permitted(caller, UniverseAction::DeleteSession, resource).await,
                expected || caller.has_role(Role::Admin),
                "{:?} {resource:?}",
                caller.roles
            );
            let stop = expected || caller.has_role(Role::Operator) || caller.has_role(Role::Admin);
            assert_eq!(
                permitted(caller, UniverseAction::StopSession, resource).await,
                stop
            );
        }
    }
    for resource in [&bot_session, &bot_child] {
        for caller in [alice, operator, universe_admin] {
            assert!(permitted(caller, UniverseAction::ControlSession, resource).await);
        }
        for caller in [bob, viewer] {
            assert!(!permitted(caller, UniverseAction::ControlSession, resource).await);
        }
    }
    for (resource, action) in [
        (&bot, UniverseAction::ManageBot),
        (&profile, UniverseAction::ManageProfile),
    ] {
        assert!(permitted(alice, action, resource).await);
        assert!(permitted(operator, action, resource).await);
        assert!(!permitted(bob, action, resource).await);
    }
    // Creation races cannot transfer a resource, and the original attribution remains.
    let race = ResourceRef::Session("race".into());
    let bob_actor = ActionActor::Principal {
        id: bob.principal.id,
    };
    let bob_controller = ResourceController::Principal(bob.principal.id);
    let (a, b) = tokio::join!(
        store.reserve_resource(universe, &race, &alice_actor, &principal, None, 5),
        store.reserve_resource(universe, &race, &bob_actor, &bob_controller, None, 5)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()),
        Some(AccessError::Denied)
    ));
    // Scope, missing lineage and cycles all fail closed.
    assert!(
        store
            .anchor(Uuid::new_v4(), &personal)
            .await
            .unwrap()
            .is_none()
    );
    let missing = ResourceRef::Session("missing".into());
    assert!(!permitted(alice, UniverseAction::ControlSession, &missing).await);
    // A controller without admitted ownership of its own cannot delegate.
    assert_eq!(
        store
            .reserve_resource(
                universe,
                &missing,
                &alice_actor,
                &ResourceController::Session("missing".into()),
                None,
                5,
            )
            .await
            .unwrap_err(),
        AccessError::Denied
    );
    assert!(!permitted(alice, UniverseAction::ControlSession, &missing).await);
    // A history fork carries provenance but has its own explicit controller.
    let fork = ResourceRef::Session("fork".into());
    store
        .reserve_resource(universe, &fork, &bob_actor, &bob_controller, None, 5)
        .await
        .unwrap();
    assert!(!permitted(alice, UniverseAction::ControlSession, &fork).await);
    assert!(permitted(bob, UniverseAction::ControlSession, &fork).await);

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
        vec![
            Read,
            ControlSession,
            StopSession,
            DeleteSession,
            ShareResource
        ]
    );
    assert_eq!(
        store
            .resource_actions(operator, &personal, false)
            .await
            .unwrap(),
        vec![Read, StopSession]
    );
    for caller in [operator, universe_admin] {
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
        vec![Read, ControlSession, StopSession, ShareResource]
    );
    assert_eq!(
        store.resource_actions(bob, &fork, true).await.unwrap(),
        vec![
            Read,
            ControlSession,
            StopSession,
            DeleteSession,
            ShareResource
        ]
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
    // Admin deletes another owner's tree; Operator does not.
    assert_eq!(
        store
            .resource_actions(universe_admin, &personal, false)
            .await
            .unwrap(),
        vec![Read, StopSession, DeleteSession]
    );

    // Restricting the root hides the whole tree from everyone but its owner,
    // Admin included; a grant on the root reaches every resource below it,
    // through group membership as well.
    sqlx::query("UPDATE access_resource_policies SET visibility='restricted' WHERE universe_id=$1 AND resource_kind='session' AND resource_id='personal'")
        .bind(universe).execute(pool).await.unwrap();
    for resource in [&personal, &child] {
        assert!(permitted(alice, Read, resource).await);
        for caller in [viewer, bob, operator, universe_admin] {
            assert_eq!(
                store
                    .decide(Caller::Request(caller), Read, resource)
                    .await
                    .unwrap(),
                Some(Decision::Hidden),
                "{:?} {resource:?}",
                caller.roles
            );
            // Governance shows through without content: Operator stops, Admin deletes.
            let governance: Vec<UniverseAction> = if caller.has_role(Role::Admin) {
                vec![StopSession, DeleteSession]
            } else if caller.has_role(Role::Operator) {
                vec![StopSession]
            } else {
                vec![]
            };
            assert_eq!(
                store
                    .resource_actions(caller, resource, false)
                    .await
                    .unwrap(),
                governance,
                "{:?}",
                caller.roles
            );
        }
    }
    sqlx::query("INSERT INTO access_resource_grants(universe_id,resource_kind,resource_id,subject_kind,subject_id,permission,granted_by,granted_at_ms) VALUES($1,'session','personal','principal',$2,'read',$3,6)")
        .bind(universe).bind(viewer.principal.id).bind(alice.principal.id).execute(pool).await.unwrap();
    let readers = Uuid::new_v4();
    store
        .apply(
            admin,
            AccessChange::CreateGroup {
                id: readers,
                display_name: "Readers".into(),
            },
            7,
        )
        .await
        .unwrap();
    store
        .apply(
            admin,
            AccessChange::PutMembership {
                membership: Membership {
                    group_id: readers,
                    principal_id: bob.principal.id,
                },
            },
            8,
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO access_resource_grants(universe_id,resource_kind,resource_id,subject_kind,subject_id,permission,granted_by,granted_at_ms) VALUES($1,'session','personal','group',$2,'write',$3,9)")
        .bind(universe).bind(readers).bind(alice.principal.id).execute(pool).await.unwrap();
    for resource in [&personal, &child] {
        assert_eq!(
            store
                .resource_actions(viewer, resource, false)
                .await
                .unwrap(),
            vec![Read]
        );
        assert_eq!(
            store.resource_actions(bob, resource, false).await.unwrap(),
            vec![Read, ControlSession, StopSession, ShareResource]
        );
        assert_eq!(
            store
                .decide(Caller::Request(operator), ControlSession, resource)
                .await
                .unwrap(),
            Some(Decision::Hidden)
        );
    }
    // The bot's worker reads its own root and universe-visible content, and
    // controls its sessions but not a stranger's.
    let worker = ControllerContext {
        universe_id: universe,
        actor: bot.clone(),
        root: bot.clone(),
        cause: "test".into(),
    };
    let controller = |action, resource: &ResourceRef| {
        let resource = resource.clone();
        let store = store.clone();
        let worker = worker.clone();
        async move {
            store
                .decide(Caller::Controller(&worker), action, &resource)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        controller(ControlSession, &bot_child).await,
        Some(Decision::Allowed)
    );
    assert_eq!(controller(Read, &fork).await, Some(Decision::Allowed));
    assert_eq!(
        controller(ControlSession, &fork).await,
        Some(Decision::Forbidden)
    );
    assert_eq!(controller(Read, &personal).await, Some(Decision::Hidden));

    // A collection is a root; a member created in it resolves to its policy
    // and takes none of its own; it cannot go while members remain.
    let collection = ResourceRef::Collection("team".into());
    store
        .reserve_resource(universe, &collection, &alice_actor, &principal, None, 11)
        .await
        .unwrap();
    store
        .create_collection(universe, "team", "Team", 11)
        .await
        .unwrap();
    let member = ResourceRef::Session("team-session".into());
    let stored = store
        .reserve_resource(
            universe,
            &member,
            &bob_actor,
            &bob_controller,
            Some(&collection),
            12,
        )
        .await
        .unwrap();
    assert_eq!(stored.audience_root, collection);
    let member_access = store
        .resource_access(universe, Some(alice.principal.id), &member)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member_access.policy.unwrap().owner, alice.principal.id);
    assert_eq!(
        store.collection_members(universe, "team").await.unwrap(),
        vec![member.clone()]
    );
    assert_eq!(
        store.delete_collection(universe, "team").await.unwrap_err(),
        AccessError::Conflict
    );
    // Only a collection can be a root, and a bot's session never names one.
    assert!(matches!(
        store
            .reserve_resource(
                universe,
                &ResourceRef::Session("x".into()),
                &bob_actor,
                &bob_controller,
                Some(&personal),
                12
            )
            .await
            .unwrap_err(),
        AccessError::Invalid(_)
    ));
    assert!(matches!(
        store
            .reserve_resource(
                universe,
                &ResourceRef::Session("y".into()),
                &alice_actor,
                &ResourceController::Bot("assistant".into()),
                Some(&collection),
                12
            )
            .await
            .unwrap_err(),
        AccessError::Invalid(_)
    ));

    // Deleting content releases its anchor, policy and grants: the id is free
    // for anyone, and the reservation of a never-created id is not adopted.
    sqlx::query("DELETE FROM sessions WHERE universe_id=$1 AND session_id='fork'")
        .bind(universe)
        .execute(pool)
        .await
        .unwrap();
    let pg = PgStore::new(pool.clone(), store_pg::PgStoreConfig::new(universe));
    use engine::storage::SessionStore as _;
    sqlx::query("UPDATE sessions SET lifecycle_status='closed', closed_at_seq=1, closed_at_ms=1, head_seq=1 WHERE universe_id=$1 AND session_id='personal'")
        .bind(universe).execute(pool).await.unwrap();
    pg.delete_closed_sessions(engine::storage::DeleteClosedSessions {
        session_id: engine::SessionId::new("personal".to_owned()),
        cascade: false,
        due_at_or_before_ms: None,
    })
    .await
    .unwrap();
    assert!(store.anchor(universe, &personal).await.unwrap().is_none());
    let leftovers: i64 = sqlx::query_scalar("SELECT count(*) FROM access_resource_grants WHERE universe_id=$1 AND resource_id='personal'")
        .bind(universe).fetch_one(pool).await.unwrap();
    assert_eq!(leftovers, 0);
    let reused = store
        .reserve_resource(universe, &personal, &bob_actor, &bob_controller, None, 10)
        .await
        .unwrap();
    assert_eq!(reused.created_by, bob_actor);

    let audit_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM access_audit_events WHERE universe_id=$1")
            .bind(universe)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        audit_events, 0,
        "preview must not record hypothetical admissions"
    );
}
