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
        CreateCollection,
        ManageCollection,
        DeleteCollection,
    ];
    let cases = [
        (
            Role::Viewer,
            vec![
                Allowed, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied,
                Denied, Denied, Denied, Denied, Denied, Denied, Denied,
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
                Denied,
                Denied,
                RequiresOwnership,
                Allowed,
                RequiresOwnership,
                RequiresOwnership,
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
                Allowed,
                RequiresOwnership,
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
                Allowed,
                RequiresOwnership,
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
    assert!(!admin.has_capability(ServiceCapability::AssertUser));
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
    effective.capabilities.insert(ServiceCapability::AssertUser);
    effective.principal.kind = PrincipalKind::Service;
    assert_eq!(
        effective.universe_action(UniverseAction::Read),
        RoleDecision::Denied
    );
    assert!(!effective.has_capability(ServiceCapability::AssertUser));
    effective.principal.status = PrincipalStatus::Active;
    effective.capabilities.clear();
    assert!(!effective.has_capability(ServiceCapability::LeaseCredentials));
    effective
        .capabilities
        .insert(ServiceCapability::LeaseCredentials);
    assert!(effective.has_capability(ServiceCapability::LeaseCredentials));
    effective.principal.kind = PrincipalKind::User;
    assert!(!effective.has_capability(ServiceCapability::LeaseCredentials));
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
        capability: ServiceCapability::ManageIdentity,
    };
    assert!(
        AccessChange::AssignCapability {
            assignment: capability
        }
        .validate()
        .is_err()
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
    // A universe-visible session: everyone reads, the owner controls,
    // elevated roles stop, only owner or Admin deletes.
    let mine = session(1, Universe, None);
    let theirs = session(7, Universe, None);
    for rights in [&viewer, &contributor, &operator, &admin] {
        assert_eq!(decide(rights, Read, &theirs), Allowed);
    }
    assert_eq!(decide(&contributor, ControlSession, &mine), Allowed);
    assert_eq!(decide(&contributor, DeleteSession, &mine), Allowed);
    assert_eq!(decide(&contributor, ControlSession, &theirs), Forbidden);
    assert_eq!(decide(&operator, ControlSession, &theirs), Forbidden);
    assert_eq!(decide(&admin, ControlSession, &theirs), Forbidden);
    assert_eq!(decide(&contributor, StopSession, &theirs), Forbidden);
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
    // A bot's session is controlled by bot managers as well as the owner.
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
    assert_eq!(
        decide(&contributor, ControlSession, &bot_session),
        Forbidden
    );
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
    // Collections: writers create in them (control), owner or Operator on a
    // visible one manages, owner or Admin deletes, Admin even when hidden.
    let collection = |owner, visibility, grant| ResourceAccess {
        anchor: anchor(
            ResourceRef::Collection("c".into()),
            ResourceRef::Collection("c".into()),
            None,
        ),
        policy: Some(policy(owner, visibility)),
        grant,
    };
    let shared = collection(7, Universe, None);
    assert_eq!(decide(&contributor, ControlSession, &shared), Forbidden);
    assert_eq!(decide(&operator, ControlSession, &shared), Forbidden);
    assert_eq!(decide(&operator, ManageCollection, &shared), Allowed);
    assert_eq!(decide(&operator, DeleteCollection, &shared), Forbidden);
    assert_eq!(decide(&admin, DeleteCollection, &shared), Allowed);
    let team = collection(7, Restricted, Some(ResourcePermission::Write));
    assert_eq!(decide(&contributor, ControlSession, &team), Allowed);
    assert_eq!(decide(&contributor, ManageCollection, &team), Forbidden);
    assert_eq!(decide(&operator, ManageCollection, &team), Forbidden);
    let hidden_collection = collection(7, Restricted, None);
    assert_eq!(
        decide(&operator, ManageCollection, &hidden_collection),
        Hidden
    );
    assert_eq!(
        decide(&admin, DeleteCollection, &hidden_collection),
        Allowed
    );
    assert_eq!(
        decide(
            &contributor,
            DeleteCollection,
            &collection(1, Restricted, None)
        ),
        Allowed
    );
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
    assert_eq!(
        authorize(Caller::Controller(&bot), UseResource, None),
        Allowed
    );
    assert_eq!(
        authorize(Caller::Controller(&parent), UseResource, None),
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
