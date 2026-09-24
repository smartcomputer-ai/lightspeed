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
    let service = Execution {
        run_as: alice.principal.id,
        kind: ExecutionKind::Service,
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
                .reserve_resource(
                    universe,
                    &record.0,
                    &alice_actor,
                    &record.1,
                    (!matches!(&record.0, ResourceRef::Profile(_))
                        && matches!(&record.1, ResourceController::Principal(_)))
                    .then_some(service),
                    None,
                    5,
                )
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
    // Execution is copied from the root to everything below it; a profile
    // runs nothing and carries none.
    for resource in [&personal, &child, &bot, &bot_session, &bot_child] {
        let anchor = store.anchor(universe, resource).await.unwrap().unwrap();
        assert_eq!(anchor.execution, Some(service), "{resource:?}");
    }
    assert_eq!(
        store
            .anchor(universe, &profile)
            .await
            .unwrap()
            .unwrap()
            .execution,
        None
    );
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
        store.reserve_resource(
            universe,
            &race,
            &alice_actor,
            &principal,
            Some(service),
            None,
            5
        ),
        store.reserve_resource(
            universe,
            &race,
            &bob_actor,
            &bob_controller,
            Some(service),
            None,
            5
        )
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
                None,
                5
            )
            .await
            .unwrap_err(),
        AccessError::Denied
    );
    assert!(!permitted(alice, UniverseAction::ControlSession, &missing).await);
    // A history fork carries provenance but has its own explicit controller.
    let fork = ResourceRef::Session("fork".into());
    store
        .reserve_resource(
            universe,
            &fork,
            &bob_actor,
            &bob_controller,
            Some(service),
            None,
            5,
        )
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
        execution_principal: Some(alice.principal.id),
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
        .reserve_resource(
            universe,
            &personal,
            &bob_actor,
            &bob_controller,
            Some(service),
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(reused.created_by, bob_actor);

    // Workspaces, environments and MCP servers are roots created by a
    // principal, anchored before their record, running nothing.
    let workspace = ResourceRef::Workspace("shared-notes".into());
    for (controller, execution) in [
        (principal.clone(), Some(service)),
        (ResourceController::Session("bot-session".into()), None),
    ] {
        assert!(matches!(
            store
                .reserve_resource(
                    universe,
                    &workspace,
                    &alice_actor,
                    &controller,
                    execution,
                    None,
                    11
                )
                .await,
            Err(AccessError::Invalid(_))
        ));
    }
    let anchored = store
        .reserve_resource(
            universe,
            &workspace,
            &alice_actor,
            &principal,
            None,
            Some(Visibility::Restricted),
            11,
        )
        .await
        .unwrap();
    assert!(anchored.is_root());
    assert_eq!(anchored.execution, None);
    // An operational id is reserved once, even by its owner: a repeated
    // create is a conflict and never rewrites the live resource's access.
    assert!(matches!(
        store
            .reserve_resource(
                universe,
                &workspace,
                &alice_actor,
                &principal,
                None,
                Some(Visibility::Universe),
                12,
            )
            .await,
        Err(AccessError::Conflict)
    ));
    // Restricted: its owner and Admin see and use it, nobody else.
    for (caller, expected) in [
        (alice, Decision::Allowed),
        (universe_admin, Decision::Allowed),
        (bob, Decision::Hidden),
        (operator, Decision::Hidden),
        (viewer, Decision::Hidden),
    ] {
        assert_eq!(
            store
                .decide(Caller::Request(caller), UseResource, &workspace)
                .await
                .unwrap(),
            Some(expected),
            "{:?}",
            caller.roles
        );
    }
    // Without an anchor it is hidden from everyone, Admin included.
    assert_eq!(
        store
            .decide(
                Caller::Request(universe_admin),
                Read,
                &ResourceRef::Environment("unanchored".into())
            )
            .await
            .unwrap(),
        Some(Decision::Hidden)
    );
    // A `use` grant lets its holder use the resource, not configure or share it.
    let grant_use = |grants: Vec<(Subject, ResourcePermission)>| PolicyReplacement {
        visibility: Visibility::Restricted,
        grants,
        owner: None,
    };
    let bob_subject = Subject::Principal(bob.principal.id);
    store
        .put_policy(
            universe,
            alice.principal.id,
            &workspace,
            &grant_use(vec![(bob_subject, ResourcePermission::Use)]),
            None,
            12,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .decide(Caller::Request(bob), UseResource, &workspace)
            .await
            .unwrap(),
        Some(Decision::Allowed)
    );
    assert_eq!(
        store
            .decide(Caller::Request(bob), ConfigureResource, &workspace)
            .await
            .unwrap(),
        Some(Decision::Forbidden)
    );
    assert!(matches!(
        store
            .put_policy(
                universe,
                alice.principal.id,
                &workspace,
                &grant_use(vec![(bob_subject, ResourcePermission::Read)]),
                None,
                13
            )
            .await,
        Err(AccessError::Invalid(_))
    ));
    assert_eq!(
        store
            .put_policy(
                universe,
                bob.principal.id,
                &workspace,
                &grant_use(vec![]),
                None,
                13
            )
            .await
            .unwrap_err(),
        AccessError::Denied
    );
    // Internal work is decided as its execution principal.
    for (execution_principal, expected) in [
        (Some(bob.principal.id), Decision::Allowed),
        (Some(viewer.principal.id), Decision::Hidden),
        (None, Decision::Forbidden),
    ] {
        let context = ControllerContext {
            execution_principal,
            ..worker.clone()
        };
        assert_eq!(
            store
                .decide(Caller::Controller(&context), UseResource, &workspace)
                .await
                .unwrap(),
            Some(expected)
        );
    }
    // An anchor whose record was never created is released; the id is free.
    assert!(store.release_resource(universe, &workspace).await.unwrap());
    assert!(store.anchor(universe, &workspace).await.unwrap().is_none());

    // Lists show only what the reader may see, and deleting a record
    // releases its anchor; an anchor whose record exists is never released.
    use mcp::McpRegistryStore as _;
    let server = |id: &str| mcp::PutMcpServerRecord {
        server_id: mcp::McpServerId::new(id),
        display_name: None,
        server_url: format!("https://{id}.example.com/mcp"),
        transport: mcp::RemoteMcpTransport::StreamableHttp,
        default_server_label: id.to_owned(),
        description: None,
        allowed_tools: None,
        execution: mcp::McpExecution::Provider,
        exposure: mcp::McpExposure::Inject,
        approval: mcp::McpApprovalPolicy::Never,
        defer_loading: None,
        allow_private_network: false,
        auth_policy: mcp::McpServerAuthPolicy::None,
        auth_grant_id: None,
        status: mcp::McpServerStatus::Active,
        now_ms: 14,
    };
    for (id, visibility) in [
        ("open", Visibility::Universe),
        ("closed", Visibility::Restricted),
    ] {
        store
            .reserve_resource(
                universe,
                &ResourceRef::McpServer(id.into()),
                &alice_actor,
                &principal,
                None,
                Some(visibility),
                14,
            )
            .await
            .unwrap();
        pg.put_server(server(id), None).await.unwrap();
    }
    pg.put_server(server("unanchored"), None).await.unwrap();
    let listed = |reader: store_pg::Reader| {
        let pg = pg.clone();
        async move {
            pg.list_servers_for(&reader, mcp::ListMcpServers::default())
                .await
                .unwrap()
                .into_iter()
                .map(|(record, access)| {
                    assert_eq!(access.owner, alice.principal.id);
                    assert_eq!(access.execution, None);
                    record.server_id.to_string()
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        listed(store_pg::Reader::Principal(alice.principal.id)).await,
        ["closed", "open"]
    );
    assert_eq!(
        listed(store_pg::Reader::Principal(bob.principal.id)).await,
        ["open"]
    );
    assert_eq!(
        listed(store_pg::Reader::Everything).await,
        ["closed", "open"]
    );
    assert_eq!(
        pg.list_servers(mcp::ListMcpServers::default())
            .await
            .unwrap()
            .len(),
        3
    );

    // One check decides what a root's execution identity may use: the
    // identity first, then every resource in one statement, in order, the
    // first refusal reported.
    let bob_work = ResourceRef::Session("bob-work".into());
    let viewer_work = ResourceRef::Session("viewer-work".into());
    for (root, owner) in [(&bob_work, bob), (&viewer_work, viewer)] {
        let id = owner.principal.id;
        store
            .reserve_resource(
                universe,
                root,
                &ActionActor::Principal { id },
                &ResourceController::Principal(id),
                Some(Execution {
                    run_as: id,
                    kind: ExecutionKind::Personal,
                }),
                None,
                14,
            )
            .await
            .unwrap();
    }
    let open = ResourceRef::McpServer("open".into());
    let closed = ResourceRef::McpServer("closed".into());
    let unanchored_server = ResourceRef::McpServer("unanchored".into());
    let never_created = ResourceRef::McpServer("never-created".into());
    let uses = |root: &ResourceRef, resources: &[ResourceRef], check| {
        let store = store.clone();
        let root = root.clone();
        let resources = resources.to_vec();
        async move {
            store
                .execution_use(universe, &root, &resources, check)
                .await
                .unwrap()
        }
    };
    let refused = |resource: &ResourceRef, run_as: &EffectiveAccess, decision| {
        Some(store_pg::UseRefusal::Resource {
            execution: Execution {
                run_as: run_as.principal.id,
                kind: ExecutionKind::Personal,
            },
            resource: resource.clone(),
            decision,
        })
    };
    use store_pg::UseCheck::{Admission, Continuation};
    assert_eq!(
        uses(&bob_work, std::slice::from_ref(&open), Admission).await,
        None
    );
    assert_eq!(
        uses(&bob_work, &[open.clone(), closed.clone()], Admission).await,
        refused(&closed, bob, Decision::Hidden)
    );
    store
        .put_policy(
            universe,
            alice.principal.id,
            &closed,
            &grant_use(vec![(bob_subject, ResourcePermission::Use)]),
            None,
            14,
        )
        .await
        .unwrap();
    assert_eq!(
        uses(&bob_work, &[open.clone(), closed.clone()], Continuation).await,
        None
    );
    // An identity that may not use resources at all is refused as such;
    // admitting nothing reads no identity, continuing work always does.
    assert_eq!(
        uses(&viewer_work, std::slice::from_ref(&open), Admission).await,
        Some(store_pg::UseRefusal::Identity)
    );
    assert_eq!(uses(&viewer_work, &[], Admission).await, None);
    assert_eq!(
        uses(&viewer_work, &[], Continuation).await,
        Some(store_pg::UseRefusal::Identity)
    );
    // A record without an anchor fails closed whenever it is checked.
    for check in [Admission, Continuation] {
        assert_eq!(
            uses(&bob_work, std::slice::from_ref(&unanchored_server), check).await,
            refused(&unanchored_server, bob, Decision::Hidden)
        );
    }
    // A missing resource cannot be attached, but one that is gone revokes
    // nothing: work that attached it continues without it.
    assert_eq!(
        uses(&bob_work, &[never_created.clone(), open.clone()], Admission).await,
        refused(&never_created, bob, Decision::Hidden)
    );
    assert_eq!(
        uses(
            &bob_work,
            &[never_created.clone(), open.clone()],
            Continuation
        )
        .await,
        None
    );
    assert!(!store.release_resource(universe, &closed).await.unwrap());
    pg.delete_server(&mcp::McpServerId::new("closed"))
        .await
        .unwrap();
    assert!(store.anchor(universe, &closed).await.unwrap().is_none());
    assert_eq!(
        uses(&bob_work, std::slice::from_ref(&closed), Continuation).await,
        None
    );
    assert_eq!(
        uses(&bob_work, std::slice::from_ref(&closed), Admission).await,
        refused(&closed, bob, Decision::Hidden)
    );

    // A writer revoked after reading the policy cannot commit a replacement
    // restoring its grant, with or without the revision it read.
    let shared = ResourceRef::Session("shared".into());
    store
        .reserve_resource(
            universe,
            &shared,
            &alice_actor,
            &principal,
            Some(service),
            None,
            15,
        )
        .await
        .unwrap();
    let session_policy =
        |visibility, grants: Vec<(Subject, ResourcePermission)>| PolicyReplacement {
            visibility,
            grants,
            owner: None,
        };
    let bob_writes = vec![(bob_subject, ResourcePermission::Write)];
    let granted = store
        .put_policy(
            universe,
            alice.principal.id,
            &shared,
            &session_policy(Visibility::Restricted, bob_writes.clone()),
            None,
            16,
        )
        .await
        .unwrap();
    store
        .put_policy(
            universe,
            alice.principal.id,
            &shared,
            &session_policy(Visibility::Restricted, vec![]),
            None,
            17,
        )
        .await
        .unwrap();
    for expected_revision in [None, Some(granted.policy.revision)] {
        assert_eq!(
            store
                .put_policy(
                    universe,
                    bob.principal.id,
                    &shared,
                    &session_policy(Visibility::Restricted, bob_writes.clone()),
                    expected_revision,
                    18,
                )
                .await
                .unwrap_err(),
            AccessError::Denied
        );
    }
    // The same holds when the revocation is still in flight: a writer's
    // replacement that waits on the owner's lock decides from what the
    // owner committed, not from what it read before waiting.
    store
        .put_policy(
            universe,
            alice.principal.id,
            &shared,
            &session_policy(Visibility::Restricted, bob_writes.clone()),
            None,
            18,
        )
        .await
        .unwrap();
    let mut revocation = pool.begin().await.unwrap();
    sqlx::query("UPDATE access_resource_policies SET revision=revision+1 WHERE universe_id=$1 AND resource_kind='session' AND resource_id='shared'")
        .bind(universe).execute(&mut *revocation).await.unwrap();
    sqlx::query("DELETE FROM access_resource_grants WHERE universe_id=$1 AND resource_kind='session' AND resource_id='shared'")
        .bind(universe).execute(&mut *revocation).await.unwrap();
    let replacement = session_policy(Visibility::Universe, bob_writes.clone());
    let write = store.put_policy(universe, bob.principal.id, &shared, &replacement, None, 19);
    let revoke = async {
        // Commit only once the writer waits on the policy row.
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database()
                   AND wait_event_type = 'Lock' AND query LIKE '%access_resource_policies%FOR UPDATE%')",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
        revocation.commit().await.unwrap();
    };
    let (written, ()) = tokio::join!(write, revoke);
    assert_eq!(written.unwrap_err(), AccessError::Denied);
    assert!(
        store
            .read_policy(universe, &shared)
            .await
            .unwrap()
            .unwrap()
            .grants
            .is_empty()
    );

    // The default agent identity reads and uses, and never gets a key.
    let execution = store.universe_execution_policy(universe, 19).await.unwrap();
    let agent = store
        .effective_access(execution.execution_principal_id, scope)
        .await
        .unwrap();
    assert_eq!(agent.principal.display_name, "Default agent identity");
    assert_eq!(
        agent.roles,
        std::collections::BTreeSet::from([Role::Executor])
    );
    use auth::ApiKeyStore as _;
    assert_eq!(
        store_pg::PgApiKeyStore::new(pool.clone())
            .create_api_key(auth::CreateApiKey {
                authority_scope: scope,
                key_hash: "0".repeat(64),
                record: auth::ApiKeyRecord {
                    key_prefix: "lsk_agent".into(),
                    scope,
                    principal_id: agent.principal.id,
                    created_by: universe_admin.principal.id,
                    display_name: None,
                    created_at_ms: 19,
                    revoked_at_ms: None,
                    last_used_at_ms: None,
                },
            })
            .await
            .unwrap_err(),
        auth::ApiKeyError::Denied
    );
    assert!(matches!(
        store
            .apply(
                admin,
                AccessChange::RevokeRole {
                    assignment: RoleAssignment {
                        scope,
                        subject: Subject::Principal(agent.principal.id),
                        role: Role::Executor,
                    },
                },
                20,
            )
            .await,
        Err(AccessError::Invalid(_))
    ));
    // It holds Executor and nothing else: no role, group or capability is
    // ever added to it, whoever asks.
    let agent_id = agent.principal.id;
    let group = Uuid::new_v4();
    store
        .apply(
            admin,
            AccessChange::CreateGroup {
                id: group,
                display_name: "Agents".into(),
            },
            20,
        )
        .await
        .unwrap();
    for change in [
        AccessChange::AssignRole {
            assignment: RoleAssignment {
                scope,
                subject: Subject::Principal(agent_id),
                role: Role::Contributor,
            },
        },
        AccessChange::PutMembership {
            membership: Membership {
                group_id: group,
                principal_id: agent_id,
            },
        },
        AccessChange::AssignCapability {
            assignment: CapabilityAssignment {
                scope,
                principal_id: agent_id,
                capability: Capability::LeaseCredentials,
            },
        },
    ] {
        assert!(
            matches!(
                store.apply(admin, change.clone(), 21).await,
                Err(AccessError::Invalid(_))
            ),
            "{change:?}"
        );
    }
    assert_eq!(
        store.effective_access(agent_id, scope).await.unwrap().roles,
        std::collections::BTreeSet::from([Role::Executor])
    );
    // It uses resources through `use` grants; it never owns a root or
    // reads or controls one.
    let agent_subject = Subject::Principal(agent_id);
    for (resource, replacement) in [
        (
            &shared,
            session_policy(
                Visibility::Restricted,
                vec![(agent_subject, ResourcePermission::Read)],
            ),
        ),
        (
            &shared,
            session_policy(
                Visibility::Restricted,
                vec![(agent_subject, ResourcePermission::Write)],
            ),
        ),
        (
            &shared,
            PolicyReplacement {
                owner: Some(agent_id),
                ..session_policy(Visibility::Restricted, vec![])
            },
        ),
    ] {
        assert!(matches!(
            store
                .put_policy(
                    universe,
                    alice.principal.id,
                    resource,
                    &replacement,
                    None,
                    22
                )
                .await,
            Err(AccessError::Invalid(_))
        ));
    }
    store
        .put_policy(
            universe,
            alice.principal.id,
            &open,
            &grant_use(vec![(agent_subject, ResourcePermission::Use)]),
            None,
            22,
        )
        .await
        .unwrap();
    // Disabling it is how an Admin stops everything that runs as it.
    let agent_work = ResourceRef::Session("agent-work".into());
    store
        .reserve_resource(
            universe,
            &agent_work,
            &alice_actor,
            &principal,
            Some(Execution {
                run_as: agent_id,
                kind: ExecutionKind::Service,
            }),
            None,
            23,
        )
        .await
        .unwrap();
    assert_eq!(
        uses(&agent_work, std::slice::from_ref(&open), Continuation).await,
        None
    );
    store
        .apply(
            admin,
            AccessChange::SetPrincipalStatus {
                id: agent_id,
                status: PrincipalStatus::Disabled,
            },
            24,
        )
        .await
        .unwrap();
    assert_eq!(
        uses(&agent_work, std::slice::from_ref(&open), Continuation).await,
        Some(store_pg::UseRefusal::Identity)
    );

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
