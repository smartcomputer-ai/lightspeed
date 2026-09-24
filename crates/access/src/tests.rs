use super::*;

fn access(role: Role, scope: AccessScope) -> EffectiveAccess {
    EffectiveAccess {
        principal: Principal {
            id: Uuid::from_u128(1),
            kind: PrincipalKind::User,
            status: PrincipalStatus::Active,
            display_name: "Alice".into(),
            management_scope: AccessScope::Deployment,
            created_at_ms: 0,
        },
        scope,
        roles: BTreeSet::from([role]),
        capabilities: BTreeSet::new(),
        policy_revision: 1,
    }
}
fn universe() -> AccessScope {
    AccessScope::Universe {
        universe_id: Uuid::from_u128(2),
    }
}

#[test]
fn role_matrix_preserves_personal_control_and_operator_scope() {
    use RoleDecision::*;
    use UniverseAction::*;
    let actions = [
        Read,
        CreateSession,
        ControlSession,
        StopSession,
        DeleteSession,
        CreateProfile,
        ManageProfile,
        CreateBot,
        ManageBot,
        InvokeBot,
        UseResource,
        ConfigureResource,
        ManageAccess,
        ShareResource,
        CreateWorkspace,
    ];
    let cases = [
        (
            Role::Viewer,
            vec![
                Allowed, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied,
                Denied, Denied, Denied, Denied, Denied,
            ],
        ),
        (
            Role::Contributor,
            vec![
                Allowed,
                Allowed,
                RequiresOwnership,
                RequiresOwnership,
                RequiresOwnership,
                Allowed,
                RequiresOwnership,
                Allowed,
                RequiresOwnership,
                Allowed,
                Allowed,
                RequiresOwnership,
                Denied,
                RequiresOwnership,
                Allowed,
            ],
        ),
        (
            Role::Operator,
            vec![
                Allowed,
                Allowed,
                RequiresOwnership,
                Allowed,
                RequiresOwnership,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Denied,
                RequiresOwnership,
                Allowed,
            ],
        ),
        (
            Role::Admin,
            vec![
                Allowed,
                Allowed,
                RequiresOwnership,
                Allowed,
                RequiresOwnership,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                Allowed,
                RequiresOwnership,
                Allowed,
            ],
        ),
        // An agent identity reads and uses; it is outside the chain above.
        (
            Role::Executor,
            vec![
                Allowed, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied,
                Allowed, Denied, Denied, Denied, Denied,
            ],
        ),
    ];
    for (role, expected) in cases {
        for (action, expected) in actions.into_iter().zip(expected) {
            assert_eq!(
                access(role, universe()).universe_action(action),
                expected,
                "{role:?} / {action:?}"
            );
        }
    }
}

#[test]
fn deployment_admin_does_not_imply_universe_membership_or_service_capabilities() {
    let admin = access(Role::DeploymentAdmin, AccessScope::Deployment);
    assert!(admin.has_role(Role::DeploymentAdmin));
    assert_eq!(
        admin.universe_action(UniverseAction::Read),
        RoleDecision::Denied
    );
    assert!(!admin.has_capability(Capability::AssertUser));
    let misplaced = access(Role::DeploymentAdmin, universe());
    assert_eq!(
        misplaced.universe_action(UniverseAction::Read),
        RoleDecision::Denied
    );
    assert!(!misplaced.has_role(Role::DeploymentAdmin));
}

#[test]
fn disabled_identity_and_service_kind_alone_convey_no_rights() {
    let mut effective = access(Role::Admin, universe());
    effective.principal.status = PrincipalStatus::Disabled;
    effective.capabilities.insert(Capability::AssertUser);
    effective.principal.kind = PrincipalKind::Service;
    assert_eq!(
        effective.universe_action(UniverseAction::Read),
        RoleDecision::Denied
    );
    assert!(!effective.has_capability(Capability::AssertUser));
    effective.principal.status = PrincipalStatus::Active;
    effective.capabilities.clear();
    assert!(!effective.has_capability(Capability::LeaseCredentials));
    effective.capabilities.insert(Capability::LeaseCredentials);
    assert!(effective.has_capability(Capability::LeaseCredentials));
    effective.principal.kind = PrincipalKind::User;
    assert!(!effective.has_capability(Capability::LeaseCredentials));
    // Privileged reading is a person's accountable act; a service never
    // holds it, and it is universe-scoped.
    effective
        .capabilities
        .insert(Capability::ReadPrivateContent);
    assert!(effective.has_capability(Capability::ReadPrivateContent));
    effective.principal.kind = PrincipalKind::Service;
    assert!(!effective.has_capability(Capability::ReadPrivateContent));
    effective.principal.kind = PrincipalKind::User;
    effective.scope = AccessScope::Deployment;
    assert!(!effective.has_capability(Capability::ReadPrivateContent));
}

#[test]
fn privileged_reading_reads_restricted_content_and_nothing_more() {
    use Decision::*;
    use UniverseAction::*;
    use Visibility::*;
    let decide = |rights: &EffectiveAccess, action, resource: &ResourceAccess| {
        authorize(Caller::Request(rights), action, Some(resource))
    };
    let mut viewer = access(Role::Viewer, universe());
    viewer.capabilities.insert(Capability::ReadPrivateContent);
    let mut contributor = access(Role::Contributor, universe());
    contributor
        .capabilities
        .insert(Capability::ReadPrivateContent);
    let mut admin = access(Role::Admin, universe());
    admin.capabilities.insert(Capability::ReadPrivateContent);
    let private = session(7, Restricted, None);
    // The read is distinguishable from an ordinary one, so it can be audited;
    // everything else is refused rather than hidden, since the holder sees
    // the resource.
    for rights in [&viewer, &contributor, &admin] {
        assert_eq!(decide(rights, Read, &private), Privileged);
        assert_eq!(decide(rights, ControlSession, &private), Forbidden);
        assert_eq!(decide(rights, ShareResource, &private), Forbidden);
    }
    assert_eq!(decide(&contributor, DeleteSession, &private), Forbidden);
    assert_eq!(decide(&contributor, StopSession, &private), Forbidden);
    // Governance by role is unchanged and never privileged.
    assert_eq!(decide(&admin, StopSession, &private), Allowed);
    assert_eq!(decide(&admin, DeleteSession, &private), Allowed);
    // Content the holder may read anyway is an ordinary read.
    assert_eq!(decide(&viewer, Read, &session(7, Universe, None)), Allowed);
    assert_eq!(
        decide(
            &viewer,
            Read,
            &session(7, Restricted, Some(ResourcePermission::Read))
        ),
        Allowed
    );
    assert_eq!(
        decide(&contributor, Read, &session(1, Restricted, None)),
        Allowed
    );
    // The capability adds nothing to a caller who may not read the
    // universe, a service, or a resource without a policy row.
    let mut no_role = access(Role::Viewer, universe());
    no_role.roles.clear();
    no_role.capabilities.insert(Capability::ReadPrivateContent);
    assert_eq!(decide(&no_role, Read, &private), Hidden);
    let mut service = viewer.clone();
    service.principal.kind = PrincipalKind::Service;
    assert_eq!(decide(&service, Read, &private), Hidden);
    let orphan = ResourceAccess {
        policy: None,
        ..session(7, Restricted, None)
    };
    assert_eq!(decide(&viewer, Read, &orphan), Hidden);
    assert!(Privileged.allows() && Allowed.allows());
    assert!(!Forbidden.allows() && !Hidden.allows());
}

#[test]
fn role_and_capability_assignments_cannot_cross_scope_classes() {
    let mut role = RoleAssignment {
        scope: universe(),
        subject: Subject::Principal(Uuid::from_u128(1)),
        role: Role::DeploymentAdmin,
    };
    assert!(matches!(
        AccessChange::AssignRole { assignment: role }.validate(),
        Err(AccessError::Invalid(_))
    ));
    role.scope = AccessScope::Deployment;
    assert!(
        AccessChange::AssignRole { assignment: role }
            .validate()
            .is_ok()
    );
    role.role = Role::Admin;
    assert!(
        AccessChange::AssignRole { assignment: role }
            .validate()
            .is_err()
    );
    let capability = CapabilityAssignment {
        scope: universe(),
        principal_id: Uuid::from_u128(1),
        capability: Capability::ManageIdentity,
    };
    assert!(
        AccessChange::AssignCapability {
            assignment: capability
        }
        .validate()
        .is_err()
    );
    let privileged = CapabilityAssignment {
        scope: AccessScope::Deployment,
        principal_id: Uuid::from_u128(1),
        capability: Capability::ReadPrivateContent,
    };
    assert!(
        AccessChange::AssignCapability {
            assignment: privileged
        }
        .validate()
        .is_err()
    );
    assert!(
        AccessChange::AssignCapability {
            assignment: CapabilityAssignment {
                scope: universe(),
                ..privileged
            }
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn replacement_roles_must_belong_to_the_existing_scope() {
    let assignment = RoleAssignment {
        scope: universe(),
        subject: Subject::Group(Uuid::from_u128(3)),
        role: Role::Viewer,
    };
    assert!(
        AccessChange::ReplaceRole {
            assignment,
            role: Role::Operator
        }
        .validate()
        .is_ok()
    );
    assert!(matches!(
        AccessChange::ReplaceRole {
            assignment,
            role: Role::DeploymentAdmin
        }
        .validate(),
        Err(AccessError::Invalid(_))
    ));
    assert!(matches!(
        AccessChange::ReplaceRole {
            assignment: RoleAssignment {
                role: Role::DeploymentAdmin,
                ..assignment
            },
            role: Role::Admin,
        }
        .validate(),
        Err(AccessError::Invalid(_))
    ));
}

#[test]
fn identity_inputs_are_explicit_and_validated() {
    let nil = AccessChange::CreateGroup {
        id: Uuid::nil(),
        display_name: "People".into(),
    };
    assert!(nil.validate().is_err());
    let empty = AccessChange::CreateGroup {
        id: Uuid::from_u128(1),
        display_name: " \n".into(),
    };
    assert!(empty.validate().is_err());
    assert!(serde_json::from_str::<Principal>(r#"{"kind":"universe_default"}"#).is_err());
    assert!(serde_json::from_str::<AccessChange>(r#"{"operation":"set_principal_status","id":"00000000-0000-0000-0000-000000000001","status":"active","extra":true}"#).is_err());
}

#[test]
fn universe_slugs_match_the_storage_boundary() {
    for slug in ["", "space here", "/path", "ä", "-leading", &"a".repeat(129)] {
        assert!(
            AccessChange::CreateUniverse {
                universe_id: Uuid::from_u128(1),
                slug: Some(slug.into())
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        AccessChange::CreateUniverse {
            universe_id: Uuid::from_u128(1),
            slug: Some("Finance_1.eu:prod-test".into())
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn credential_scope_is_a_ceiling() {
    let other = AccessScope::Universe {
        universe_id: Uuid::from_u128(9),
    };
    assert!(AccessScope::Deployment.permits(universe()));
    assert!(AccessScope::Deployment.permits(AccessScope::Deployment));
    assert!(universe().permits(universe()));
    assert!(!universe().permits(other));
    assert!(!universe().permits(AccessScope::Deployment));
}

fn anchor(resource: ResourceRef, root: ResourceRef, bot: Option<&str>) -> ResourceAnchor {
    ResourceAnchor {
        resource,
        created_by: ActionActor::Principal {
            id: Uuid::from_u128(7),
        },
        controller: ResourceController::Principal(Uuid::from_u128(7)),
        audience_root: root,
        bot: bot.map(str::to_owned),
        execution: None,
        created_at_ms: 0,
    }
}

fn policy(owner: u128, visibility: Visibility) -> ResourcePolicy {
    ResourcePolicy {
        owner: Uuid::from_u128(owner),
        visibility,
        revision: 1,
        updated_by: ActionActor::Principal {
            id: Uuid::from_u128(owner),
        },
        updated_at_ms: 0,
    }
}

fn session(
    owner: u128,
    visibility: Visibility,
    grant: Option<ResourcePermission>,
) -> ResourceAccess {
    let id = ResourceRef::Session("s".into());
    ResourceAccess {
        anchor: anchor(id.clone(), id, None),
        policy: Some(policy(owner, visibility)),
        grant,
    }
}

#[test]
fn resource_decisions_follow_owner_visibility_grant_and_role() {
    use Decision::*;
    use UniverseAction::*;
    use Visibility::*;
    let viewer = access(Role::Viewer, universe());
    let contributor = access(Role::Contributor, universe());
    let operator = access(Role::Operator, universe());
    let admin = access(Role::Admin, universe());
    let decide = |rights: &EffectiveAccess, action, resource: &ResourceAccess| {
        authorize(Caller::Request(rights), action, Some(resource))
    };
    // A universe-visible session is shared work: everyone reads, every
    // Contributor and above works in it and stops it, only the owner or an
    // Admin deletes it, only the owner or a writer shares it.
    let mine = session(1, Universe, None);
    let theirs = session(7, Universe, None);
    for rights in [&viewer, &contributor, &operator, &admin] {
        assert_eq!(decide(rights, Read, &theirs), Allowed);
    }
    assert_eq!(decide(&contributor, ControlSession, &mine), Allowed);
    assert_eq!(decide(&contributor, DeleteSession, &mine), Allowed);
    for rights in [&contributor, &operator, &admin] {
        assert_eq!(decide(rights, ControlSession, &theirs), Allowed);
        assert_eq!(decide(rights, StopSession, &theirs), Allowed);
    }
    assert_eq!(decide(&contributor, DeleteSession, &theirs), Forbidden);
    assert_eq!(decide(&operator, StopSession, &theirs), Allowed);
    assert_eq!(decide(&operator, DeleteSession, &theirs), Forbidden);
    assert_eq!(decide(&admin, DeleteSession, &theirs), Allowed);
    assert_eq!(decide(&viewer, ControlSession, &theirs), Forbidden);
    // A restricted session is absent to everyone without a grant, Admin
    // included; a reader sees it, a writer controls and stops it, neither
    // deletes it.
    let private = session(7, Restricted, None);
    for rights in [&viewer, &contributor, &operator, &admin] {
        assert_eq!(decide(rights, Read, &private), Hidden, "{:?}", rights.roles);
        assert_eq!(decide(rights, ControlSession, &private), Hidden);
        assert_eq!(decide(rights, ShareResource, &private), Hidden);
    }
    // Governance needs no content: Operator/Admin stop, Admin deletes.
    assert_eq!(decide(&contributor, StopSession, &private), Hidden);
    assert_eq!(decide(&operator, StopSession, &private), Allowed);
    assert_eq!(decide(&operator, DeleteSession, &private), Hidden);
    assert_eq!(decide(&admin, StopSession, &private), Allowed);
    assert_eq!(decide(&admin, DeleteSession, &private), Allowed);
    assert_eq!(
        decide(&contributor, Read, &session(1, Restricted, None)),
        Allowed
    );
    let shared_read = session(7, Restricted, Some(ResourcePermission::Read));
    assert_eq!(decide(&viewer, Read, &shared_read), Allowed);
    assert_eq!(
        decide(&contributor, ControlSession, &shared_read),
        Forbidden
    );
    let shared_write = session(7, Restricted, Some(ResourcePermission::Write));
    assert_eq!(decide(&contributor, ControlSession, &shared_write), Allowed);
    assert_eq!(decide(&contributor, StopSession, &shared_write), Allowed);
    assert_eq!(
        decide(&contributor, DeleteSession, &shared_write),
        Forbidden
    );
    // A grant never widens a role: a viewer with write still cannot control.
    assert_eq!(decide(&viewer, ControlSession, &shared_write), Forbidden);
    // Sharing follows control: owner or writer, never a reader or a role.
    assert_eq!(decide(&contributor, ShareResource, &mine), Allowed);
    assert_eq!(decide(&contributor, ShareResource, &shared_write), Allowed);
    assert_eq!(decide(&contributor, ShareResource, &shared_read), Forbidden);
    assert_eq!(decide(&admin, ShareResource, &theirs), Forbidden);
    assert_eq!(decide(&viewer, ShareResource, &shared_write), Forbidden);
    // A missing policy row denies outright.
    let orphan = ResourceAccess {
        policy: None,
        ..session(1, Universe, None)
    };
    assert_eq!(decide(&admin, Read, &orphan), Hidden);
    // A universe-visible bot's sessions are shared work too; bot managers
    // also delete them.
    let bot_session = ResourceAccess {
        anchor: anchor(
            ResourceRef::Session("s".into()),
            ResourceRef::Bot("b".into()),
            Some("b"),
        ),
        policy: Some(policy(7, Universe)),
        grant: None,
    };
    assert_eq!(decide(&operator, ControlSession, &bot_session), Allowed);
    assert_eq!(decide(&operator, DeleteSession, &bot_session), Allowed);
    assert_eq!(decide(&contributor, ControlSession, &bot_session), Allowed);
    assert_eq!(decide(&contributor, DeleteSession, &bot_session), Forbidden);
    // Bots: invocation follows the role on a universe-visible root, managing
    // follows ownership or the Operator role on a universe-visible root.
    let bot = ResourceAccess {
        anchor: anchor(
            ResourceRef::Bot("b".into()),
            ResourceRef::Bot("b".into()),
            None,
        ),
        policy: Some(policy(7, Universe)),
        grant: None,
    };
    assert_eq!(decide(&contributor, InvokeBot, &bot), Allowed);
    assert_eq!(decide(&viewer, InvokeBot, &bot), Forbidden);
    assert_eq!(decide(&contributor, ManageBot, &bot), Forbidden);
    assert_eq!(decide(&operator, ManageBot, &bot), Allowed);
    let private_bot = ResourceAccess {
        policy: Some(policy(7, Restricted)),
        grant: Some(ResourcePermission::Write),
        ..bot.clone()
    };
    assert_eq!(decide(&operator, ManageBot, &private_bot), Forbidden);
    assert_eq!(decide(&contributor, InvokeBot, &private_bot), Allowed);
    // Personal work runs as its owner: whatever its visibility, only the
    // owner (or a writer the owner added) works in it or configures it.
    // Governance stays: Operators stop it, Admins delete it.
    let personal = |resource: ResourceRef, root: ResourceRef, bot: Option<&str>| ResourceAccess {
        anchor: ResourceAnchor {
            execution: Some(Execution {
                run_as: Uuid::from_u128(7),
                kind: ExecutionKind::Personal,
            }),
            ..anchor(resource, root, bot)
        },
        policy: Some(policy(7, Universe)),
        grant: None,
    };
    let personal_session = personal(
        ResourceRef::Session("p".into()),
        ResourceRef::Session("p".into()),
        None,
    );
    assert_eq!(decide(&contributor, Read, &personal_session), Allowed);
    for rights in [&contributor, &operator, &admin] {
        assert_eq!(decide(rights, ControlSession, &personal_session), Forbidden);
    }
    assert_eq!(
        decide(&contributor, StopSession, &personal_session),
        Forbidden
    );
    assert_eq!(decide(&operator, StopSession, &personal_session), Allowed);
    assert_eq!(decide(&admin, DeleteSession, &personal_session), Allowed);
    let personal_bot = personal(
        ResourceRef::Bot("pb".into()),
        ResourceRef::Bot("pb".into()),
        None,
    );
    assert_eq!(decide(&contributor, InvokeBot, &personal_bot), Forbidden);
    assert_eq!(decide(&operator, ManageBot, &personal_bot), Forbidden);
    let personal_bot_session = personal(
        ResourceRef::Session("ps".into()),
        ResourceRef::Bot("pb".into()),
        Some("pb"),
    );
    assert_eq!(
        decide(&operator, ControlSession, &personal_bot_session),
        Forbidden
    );
    let mut owner = access(Role::Contributor, universe());
    owner.principal.id = Uuid::from_u128(7);
    assert_eq!(decide(&owner, ControlSession, &personal_session), Allowed);
    assert_eq!(decide(&owner, InvokeBot, &personal_bot), Allowed);
    assert_eq!(decide(&owner, ManageBot, &personal_bot), Allowed);
    // Without a target, only the role decides.
    assert_eq!(
        authorize(Caller::Request(&contributor), CreateSession, None),
        Allowed
    );
    assert_eq!(
        authorize(Caller::Request(&contributor), ControlSession, None),
        Forbidden
    );
}

#[test]
fn controller_contexts_control_themselves_their_bots_sessions_and_admitted_children() {
    use Decision::*;
    use UniverseAction::*;
    let bot = ControllerContext {
        universe_id: Uuid::from_u128(2),
        actor: ResourceRef::Bot("b".into()),
        root: ResourceRef::Bot("b".into()),
        execution_principal: Some(Uuid::from_u128(9)),
        cause: "test".into(),
    };
    let parent = ControllerContext {
        actor: ResourceRef::Session("p".into()),
        root: ResourceRef::Session("p".into()),
        ..bot.clone()
    };
    let with =
        |resource: ResourceRef, root: ResourceRef, bot_id: Option<&str>, controller, visibility| {
            ResourceAccess {
                anchor: ResourceAnchor {
                    controller,
                    ..anchor(resource, root, bot_id)
                },
                policy: Some(policy(7, visibility)),
                grant: None,
            }
        };
    let bot_session = with(
        ResourceRef::Session("s".into()),
        ResourceRef::Bot("b".into()),
        Some("b"),
        ResourceController::Bot("b".into()),
        Visibility::Restricted,
    );
    let child = with(
        ResourceRef::Session("c".into()),
        ResourceRef::Session("p".into()),
        None,
        ResourceController::Session("p".into()),
        Visibility::Restricted,
    );
    let stranger = with(
        ResourceRef::Session("x".into()),
        ResourceRef::Session("x".into()),
        None,
        ResourceController::Principal(Uuid::from_u128(7)),
        Visibility::Universe,
    );
    let hidden = with(
        ResourceRef::Session("y".into()),
        ResourceRef::Session("y".into()),
        None,
        ResourceController::Principal(Uuid::from_u128(7)),
        Visibility::Restricted,
    );
    let decide =
        |context, action, resource| authorize(Caller::Controller(context), action, Some(resource));
    assert_eq!(decide(&bot, ControlSession, &bot_session), Allowed);
    assert_eq!(decide(&bot, ControlSession, &child), Hidden);
    assert_eq!(decide(&bot, ControlSession, &stranger), Forbidden);
    assert_eq!(decide(&bot, Read, &stranger), Allowed);
    assert_eq!(decide(&bot, Read, &hidden), Hidden);
    assert_eq!(decide(&parent, ControlSession, &child), Allowed);
    assert_eq!(decide(&parent, Read, &child), Allowed);
    assert_eq!(decide(&parent, ControlSession, &bot_session), Hidden);
    assert_eq!(decide(&parent, ControlSession, &stranger), Forbidden);
    assert_eq!(decide(&parent, ManageAccess, &child), Forbidden);
    // A sibling under the same root is readable, never controlled.
    let sibling = with(
        ResourceRef::Session("c2".into()),
        ResourceRef::Session("p".into()),
        None,
        ResourceController::Session("c".into()),
        Visibility::Restricted,
    );
    assert_eq!(decide(&parent, Read, &sibling), Allowed);
    assert_eq!(decide(&parent, ControlSession, &sibling), Forbidden);
    // Resource use follows the execution principal, not the actor's kind.
    assert_eq!(
        authorize(Caller::Controller(&bot), UseResource, None),
        Allowed
    );
    assert_eq!(
        authorize(Caller::Controller(&parent), UseResource, None),
        Allowed
    );
    let unexecuted = ControllerContext {
        execution_principal: None,
        ..parent.clone()
    };
    assert_eq!(
        authorize(Caller::Controller(&unexecuted), UseResource, None),
        Forbidden
    );
    assert_eq!(
        authorize(Caller::Controller(&parent), CreateSession, None),
        Allowed
    );
}

#[test]
fn key_issuance_respects_ceiling_self_service_and_management_scope() {
    let service = |scope| Principal {
        id: Uuid::from_u128(5),
        kind: PrincipalKind::Service,
        status: PrincipalStatus::Active,
        display_name: "svc".into(),
        management_scope: scope,
        created_at_ms: 0,
    };
    let contributor = access(Role::Contributor, universe());
    let admin = access(Role::Admin, universe());
    let none = EffectiveAccess {
        roles: BTreeSet::new(),
        ..access(Role::Viewer, AccessScope::Deployment)
    };
    // Members mint their own universe key; a roleless principal cannot.
    assert!(may_issue_key(
        universe(),
        &contributor,
        &none,
        &contributor.principal
    ));
    let roleless = EffectiveAccess {
        roles: BTreeSet::new(),
        ..contributor.clone()
    };
    assert!(!may_issue_key(
        universe(),
        &roleless,
        &none,
        &roleless.principal
    ));
    // Only an Admin mints for a service, and only one its universe manages.
    assert!(!may_issue_key(
        universe(),
        &contributor,
        &none,
        &service(universe())
    ));
    assert!(may_issue_key(
        universe(),
        &admin,
        &none,
        &service(universe())
    ));
    assert!(!may_issue_key(
        universe(),
        &admin,
        &none,
        &service(AccessScope::Deployment)
    ));
    // A universe credential never reaches another scope.
    let elsewhere = AccessScope::Universe {
        universe_id: Uuid::from_u128(9),
    };
    assert!(!may_issue_key(
        elsewhere,
        &admin,
        &none,
        &service(universe())
    ));
    // Deployment keys need a deployment credential and DeploymentAdmin.
    let deployment_admin = access(Role::DeploymentAdmin, AccessScope::Deployment);
    assert!(may_issue_key(
        AccessScope::Deployment,
        &deployment_admin,
        &deployment_admin,
        &service(AccessScope::Deployment)
    ));
    assert!(!may_issue_key(
        universe(),
        &deployment_admin,
        &deployment_admin,
        &service(AccessScope::Deployment)
    ));
}

fn operational(
    owner: u128,
    visibility: Visibility,
    grant: Option<ResourcePermission>,
) -> ResourceAccess {
    let id = ResourceRef::Environment("e".into());
    ResourceAccess {
        anchor: anchor(id.clone(), id, None),
        policy: Some(policy(owner, visibility)),
        grant,
    }
}

#[test]
fn resource_refs_name_their_kind_and_id() {
    for (resource, kind, operational) in [
        (ResourceRef::Session("x".into()), "session", false),
        (ResourceRef::Bot("x".into()), "bot", false),
        (ResourceRef::Profile("x".into()), "profile", false),
        (ResourceRef::Workspace("x".into()), "workspace", true),
        (ResourceRef::Environment("x".into()), "environment", true),
        (ResourceRef::McpServer("x".into()), "mcp_server", true),
    ] {
        assert_eq!(resource.kind(), kind);
        assert_eq!(resource.id(), "x");
        assert_eq!(resource.is_operational(), operational);
        assert_eq!(
            ResourceRef::from_kind(kind, "x".into()),
            Some(resource.clone())
        );
        // The stored kind is the wire kind.
        assert_eq!(
            serde_json::to_value(&resource).unwrap(),
            serde_json::json!({"kind": kind, "id": "x"})
        );
    }
    assert_eq!(ResourceRef::from_kind("collection", "x".into()), None);
}

#[test]
fn grant_permissions_belong_to_their_kinds() {
    use ResourcePermission::*;
    for resource in [
        ResourceRef::Session("s".into()),
        ResourceRef::Bot("b".into()),
    ] {
        assert!(Read.valid_for(&resource) && Write.valid_for(&resource));
        assert!(!Use.valid_for(&resource));
    }
    for resource in [
        ResourceRef::Workspace("w".into()),
        ResourceRef::Environment("e".into()),
        ResourceRef::McpServer("m".into()),
    ] {
        assert!(Use.valid_for(&resource));
        assert!(!Read.valid_for(&resource) && !Write.valid_for(&resource));
    }
    let profile = ResourceRef::Profile("p".into());
    assert!(![Read, Write, Use].iter().any(|p| p.valid_for(&profile)));
    assert_eq!(serde_json::to_value(Use).unwrap(), "use");
}

#[test]
fn executor_is_never_assigned_or_revoked_by_identity_changes() {
    let executor = RoleAssignment {
        scope: universe(),
        subject: Subject::Principal(Uuid::from_u128(1)),
        role: Role::Executor,
    };
    let viewer = RoleAssignment {
        role: Role::Viewer,
        ..executor
    };
    for change in [
        AccessChange::AssignRole {
            assignment: executor,
        },
        AccessChange::RevokeRole {
            assignment: executor,
        },
        AccessChange::ReplaceRole {
            assignment: viewer,
            role: Role::Executor,
        },
        AccessChange::ReplaceRole {
            assignment: executor,
            role: Role::Viewer,
        },
    ] {
        assert_eq!(
            change.validate(),
            Err(AccessError::Invalid("executor is system-assigned".into()))
        );
    }
    assert!(!Role::Executor.valid_in(AccessScope::Deployment));
    assert!(Role::Executor.valid_in(universe()));
    assert_eq!(serde_json::to_value(Role::Executor).unwrap(), "executor");
}

/// An agent identity holds Executor and nothing else: a role that reached it
/// anyway, directly or through a group, confers nothing.
#[test]
fn executor_alone_decides_for_an_agent_identity() {
    use UniverseAction::*;
    let executor = access(Role::Executor, universe());
    for role in [Role::Viewer, Role::Contributor, Role::Operator, Role::Admin] {
        let mut agent = executor.clone();
        agent.roles.insert(role);
        assert!(!agent.has_role(role), "{role:?}");
        assert!(agent.has_role(Role::Executor));
        for action in [
            Read,
            CreateSession,
            ControlSession,
            StopSession,
            DeleteSession,
            CreateProfile,
            ManageProfile,
            CreateBot,
            ManageBot,
            InvokeBot,
            UseResource,
            ConfigureResource,
            ManageAccess,
            ShareResource,
            CreateWorkspace,
        ] {
            assert_eq!(
                agent.universe_action(action),
                executor.universe_action(action),
                "{role:?} / {action:?}"
            );
        }
        // Admin's reach over restricted resources does not reach it either.
        let restricted = operational(7, Visibility::Restricted, None);
        for action in [Read, UseResource, ConfigureResource, ShareResource] {
            assert_eq!(
                authorize(Caller::Request(&agent), action, Some(&restricted)),
                Decision::Hidden
            );
        }
    }
}

#[test]
fn resource_labels_name_kinds_as_people_read_them() {
    assert_eq!(ResourceRef::McpServer("m".into()).label(), "MCP server");
    assert_eq!(ResourceRef::Workspace("w".into()).label(), "workspace");
    assert_eq!(ResourceRef::Environment("e".into()).label(), "environment");
    assert_eq!(ResourceRef::Session("s".into()).label(), "session");
}

/// The role defaults on a universe-visible workspace, environment or MCP
/// server, and their replacement on a restricted one: seeing and using need
/// ownership, a `use` grant or Admin; configuring and sharing need ownership
/// or Admin. Roles stay ceilings throughout.
#[test]
fn operational_resources_follow_role_visibility_grant_and_ownership() {
    use Decision::*;
    use UniverseAction::*;
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Relation {
        None,
        UseGrant,
        Owner,
    }
    let expected = |role: Role, visibility: Visibility, relation: Relation, action| {
        let admin = role == Role::Admin;
        let owner = relation == Relation::Owner;
        let universe = visibility == Visibility::Universe;
        if !(universe || relation != Relation::None || admin) {
            return Hidden;
        }
        let allowed = match action {
            Read => true,
            UseResource => role != Role::Viewer,
            ConfigureResource => match role {
                Role::Viewer | Role::Executor => false,
                Role::Contributor => owner,
                Role::Operator => owner || universe,
                _ => true,
            },
            ShareResource => match role {
                Role::Viewer | Role::Executor => false,
                Role::Contributor | Role::Operator => owner,
                _ => true,
            },
            _ => unreachable!(),
        };
        if allowed { Allowed } else { Forbidden }
    };
    for role in [
        Role::Viewer,
        Role::Contributor,
        Role::Operator,
        Role::Admin,
        Role::Executor,
    ] {
        let rights = access(role, universe());
        for visibility in [Visibility::Universe, Visibility::Restricted] {
            for relation in [Relation::None, Relation::UseGrant, Relation::Owner] {
                let resource = operational(
                    if relation == Relation::Owner { 1 } else { 7 },
                    visibility,
                    (relation == Relation::UseGrant).then_some(ResourcePermission::Use),
                );
                for action in [Read, UseResource, ConfigureResource, ShareResource] {
                    assert_eq!(
                        authorize(Caller::Request(&rights), action, Some(&resource)),
                        expected(role, visibility, relation, action),
                        "{role:?} {visibility:?} {relation:?} {action:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn operational_resources_have_no_privileged_or_governance_path() {
    use Decision::*;
    use UniverseAction::*;
    let mut viewer = access(Role::Viewer, universe());
    viewer.capabilities.insert(Capability::ReadPrivateContent);
    let restricted = operational(7, Visibility::Restricted, None);
    // Restriction is governance: privileged reading does not reach it.
    assert_eq!(
        authorize(Caller::Request(&viewer), Read, Some(&restricted)),
        Hidden
    );
    let operator = access(Role::Operator, universe());
    for action in [StopSession, DeleteSession, ControlSession, ManageAccess] {
        assert_eq!(
            authorize(Caller::Request(&operator), action, Some(&restricted)),
            Hidden
        );
        assert_eq!(
            authorize(
                Caller::Request(&operator),
                action,
                Some(&operational(7, Visibility::Universe, None))
            ),
            Forbidden
        );
    }
    // A missing policy is hidden from everyone, Admin included.
    let orphan = ResourceAccess {
        policy: None,
        ..operational(1, Visibility::Universe, None)
    };
    let admin = access(Role::Admin, universe());
    assert_eq!(
        authorize(Caller::Request(&admin), Read, Some(&orphan)),
        Hidden
    );
    // A session-kind grant on an operational resource confers no use.
    let contributor = access(Role::Contributor, universe());
    assert_eq!(
        authorize(
            Caller::Request(&contributor),
            UseResource,
            Some(&operational(
                7,
                Visibility::Restricted,
                Some(ResourcePermission::Write)
            ))
        ),
        Forbidden
    );
    // Internal work is decided as its execution principal by the store,
    // never by controller rules.
    let controller = ControllerContext {
        universe_id: Uuid::from_u128(2),
        actor: ResourceRef::Session("s".into()),
        root: ResourceRef::Session("s".into()),
        execution_principal: Some(Uuid::from_u128(9)),
        cause: "test".into(),
    };
    for action in [Read, UseResource] {
        assert_eq!(
            authorize(
                Caller::Controller(&controller),
                action,
                Some(&operational(7, Visibility::Universe, None))
            ),
            Forbidden
        );
    }
    // Creating a workspace is a Contributor's universe action.
    assert_eq!(
        authorize(Caller::Request(&contributor), CreateWorkspace, None),
        Allowed
    );
    assert_eq!(
        authorize(
            Caller::Request(&access(Role::Executor, universe())),
            CreateWorkspace,
            None
        ),
        Forbidden
    );
}

#[test]
fn policy_replacement_is_decided_from_the_locked_facts() {
    use ResourcePermission::*;
    let facts = |visibility, grants: Vec<(Subject, ResourcePermission)>, owner: Option<u128>| {
        PolicyReplacement {
            visibility,
            grants,
            owner: owner.map(Uuid::from_u128),
        }
    };
    let grant = |subject: u128, permission| ResourceGrant {
        subject: Subject::Principal(Uuid::from_u128(subject)),
        permission,
        granted_by: Uuid::from_u128(7),
        granted_at_ms: 0,
    };
    let bob = Subject::Principal(Uuid::from_u128(5));
    let carol = Subject::Principal(Uuid::from_u128(6));
    let owner = access(Role::Contributor, universe());
    let writer = access(Role::Contributor, universe());
    // A session owned by 7, on which the caller (1) is a writer.
    let written = session(7, Visibility::Restricted, Some(Write));
    let current = [grant(1, Write), grant(5, Read)];
    let caller = Subject::Principal(Uuid::from_u128(1));
    // A writer changes visibility and read grants, keeping every writer.
    assert_eq!(
        authorize_policy_replacement(
            &writer,
            &written,
            &current,
            &facts(
                Visibility::Universe,
                vec![(caller, Write), (carol, Read)],
                None
            )
        ),
        Ok(())
    );
    // It neither adds nor removes a writer, itself included.
    for grants in [
        vec![(caller, Write), (bob, Write)],
        vec![(bob, Read)],
        vec![(caller, Read), (bob, Read)],
    ] {
        assert_eq!(
            authorize_policy_replacement(
                &writer,
                &written,
                &current,
                &facts(Visibility::Restricted, grants, None)
            ),
            Err(AccessError::Denied)
        );
    }
    // A writer whose grant was revoked before the lock cannot restore it.
    let revoked = session(7, Visibility::Restricted, None);
    assert_eq!(
        authorize_policy_replacement(
            &writer,
            &revoked,
            &[grant(5, Read)],
            &facts(Visibility::Restricted, vec![(caller, Write)], None)
        ),
        Err(AccessError::Denied)
    );
    // Only the owner hands a root over, and the new owner takes no grant.
    assert_eq!(
        authorize_policy_replacement(
            &writer,
            &written,
            &current,
            &facts(Visibility::Restricted, vec![(caller, Write)], Some(5))
        ),
        Err(AccessError::Denied)
    );
    let mine = session(1, Visibility::Restricted, None);
    assert_eq!(
        authorize_policy_replacement(
            &owner,
            &mine,
            &[],
            &facts(Visibility::Restricted, vec![(caller, Write)], Some(5))
        ),
        Ok(())
    );
    assert!(matches!(
        authorize_policy_replacement(
            &owner,
            &mine,
            &[],
            &facts(Visibility::Restricted, vec![(bob, Write)], Some(5))
        ),
        Err(AccessError::Invalid(_))
    ));
    assert!(matches!(
        authorize_policy_replacement(
            &owner,
            &mine,
            &[],
            &facts(Visibility::Restricted, vec![(caller, Read)], None)
        ),
        Err(AccessError::Invalid(_))
    ));
    // Grants are permissions of the root's kind.
    assert_eq!(
        authorize_policy_replacement(
            &owner,
            &mine,
            &[],
            &facts(Visibility::Restricted, vec![(bob, Use)], None)
        ),
        Err(AccessError::Invalid("a session takes no use grant".into()))
    );
    let environment = operational(1, Visibility::Restricted, None);
    assert!(matches!(
        authorize_policy_replacement(
            &owner,
            &environment,
            &[],
            &facts(Visibility::Restricted, vec![(bob, Read)], None)
        ),
        Err(AccessError::Invalid(_))
    ));
    assert_eq!(
        authorize_policy_replacement(
            &owner,
            &environment,
            &[],
            &facts(Visibility::Restricted, vec![(bob, Use)], None)
        ),
        Ok(())
    );
    // Admin manages access to any operational resource but never takes it
    // over; a user with a `use` grant manages nothing.
    let admin = access(Role::Admin, universe());
    let theirs = operational(7, Visibility::Restricted, None);
    assert_eq!(
        authorize_policy_replacement(
            &admin,
            &theirs,
            &[],
            &facts(Visibility::Universe, vec![(bob, Use)], None)
        ),
        Ok(())
    );
    assert_eq!(
        authorize_policy_replacement(
            &admin,
            &theirs,
            &[],
            &facts(Visibility::Universe, vec![], Some(1))
        ),
        Err(AccessError::Denied)
    );
    let user = access(Role::Operator, universe());
    assert_eq!(
        authorize_policy_replacement(
            &user,
            &operational(7, Visibility::Restricted, Some(Use)),
            &[grant(1, Use)],
            &facts(Visibility::Universe, vec![(caller, Use)], None)
        ),
        Err(AccessError::Denied)
    );
    // Profiles are not shared at all.
    let profile = ResourceAccess {
        anchor: anchor(
            ResourceRef::Profile("p".into()),
            ResourceRef::Profile("p".into()),
            None,
        ),
        policy: Some(policy(1, Visibility::Universe)),
        grant: None,
    };
    assert_eq!(
        authorize_policy_replacement(
            &owner,
            &profile,
            &[],
            &facts(Visibility::Universe, vec![], None)
        ),
        Err(AccessError::Denied)
    );
}

/// The batch question every session check asks: the first resource the
/// identity may not use, in the order given, a missing anchor hidden.
#[test]
fn first_unusable_reports_the_first_refused_resource_in_order() {
    let executor = access(Role::Executor, universe());
    let with_id = |resource: ResourceRef, access: ResourceAccess| ResourceAccess {
        anchor: anchor(resource.clone(), resource, None),
        ..access
    };
    let open = ResourceRef::Workspace("open".into());
    let granted = ResourceRef::McpServer("granted".into());
    let restricted = ResourceRef::Environment("restricted".into());
    let missing = ResourceRef::Environment("missing".into());
    let accesses = [
        with_id(open.clone(), operational(7, Visibility::Universe, None)),
        with_id(
            granted.clone(),
            operational(7, Visibility::Restricted, Some(ResourcePermission::Use)),
        ),
        with_id(
            restricted.clone(),
            operational(7, Visibility::Restricted, None),
        ),
    ];
    assert_eq!(first_unusable(&executor, &[], &accesses), None);
    assert_eq!(
        first_unusable(&executor, &[open.clone(), granted.clone()], &accesses),
        None
    );
    assert_eq!(
        first_unusable(
            &executor,
            &[open.clone(), missing.clone(), restricted.clone()],
            &accesses
        ),
        Some((missing, Decision::Hidden))
    );
    assert_eq!(
        first_unusable(&executor, &[restricted.clone(), open.clone()], &accesses),
        Some((restricted, Decision::Hidden))
    );
    // A Viewer sees what is open but may use none of it.
    assert_eq!(
        first_unusable(
            &access(Role::Viewer, universe()),
            std::slice::from_ref(&open),
            &accesses
        ),
        Some((open, Decision::Forbidden))
    );
}
