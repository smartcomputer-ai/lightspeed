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
    ];
    let cases = [
        (
            Role::Viewer,
            vec![
                Allowed, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied, Denied,
                Denied, Denied, Denied,
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
