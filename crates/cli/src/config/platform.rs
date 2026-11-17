//! The Platform's part of configuration documents: `kind: organization`
//! documents, and a universe document's `organization` and `access`. The
//! runtime has no organizations or people, so these need a Platform
//! connection.
//!
//! Like the rest of `apply`, nothing is deleted: a service user or grant a
//! document leaves out is kept. Organizations and service users are found by
//! handle and name, which are the same in every deployment, and resolved again
//! when a step runs, so one run can create an organization and the universes
//! and grants that need it.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use super::document::{OrganizationDocument, ServiceUserEntry, ServiceUserGrant, UniverseAccess};
use super::plan::{Action, Step, Work};
use super::resources::Target;
use crate::connection::ResolvedConnection;

/// What the documents of one run declare, so a universe document may name an
/// organization or service user another document of the run creates.
#[derive(Debug, Default)]
pub(crate) struct Declared {
    pub organizations: BTreeSet<String>,
    /// Service user names, by organization handle.
    pub service_users: BTreeMap<String, BTreeSet<String>>,
}

impl Declared {
    pub(crate) fn of(documents: &[super::document::LoadedDocument]) -> Self {
        let mut declared = Self::default();
        for loaded in documents {
            if let super::document::Document::Organization(organization) = &loaded.document {
                declared.organizations.insert(organization.handle.clone());
                declared.service_users.insert(
                    organization.handle.clone(),
                    organization
                        .service_users
                        .iter()
                        .flatten()
                        .map(|service_user| service_user.name.clone())
                        .collect(),
                );
            }
        }
        declared
    }
}

/// A Platform write, carried out when `apply` reaches its step.
#[derive(Clone, Debug)]
pub(crate) enum PlatformWork {
    CreateOrganization {
        params: Value,
    },
    UpdateOrganization {
        handle: String,
        changes: Value,
    },
    CreateServiceUser {
        handle: String,
        name: String,
        role: String,
        enabled: Option<bool>,
    },
    UpdateServiceUser {
        handle: String,
        name: String,
        changes: Value,
    },
    PutAccessPolicy {
        policy: Value,
    },
    GrantServiceUser {
        name: String,
        role: String,
    },
    /// `credential` holds the key's name, secret, expiry and restrictions.
    PutServiceUserKey {
        handle: String,
        service_user: String,
        credential: Value,
    },
}

pub(crate) fn require_platform(base: &ResolvedConnection, what: &str) -> Result<()> {
    anyhow::ensure!(
        base.is_platform(),
        "{what} is Platform configuration; connect through Platform to apply it"
    );
    Ok(())
}

async fn call(target: &Target, operation: &str, params: Value) -> Result<Value> {
    target
        .api
        .call_platform(operation, params)
        .await
        .map_err(|error| anyhow::anyhow!("{operation}: {error}"))
}

/// The organization with this handle, as the caller sees it.
async fn organization(target: &Target, handle: &str) -> Result<Option<Value>> {
    let listed = call(target, "platform/organizations/list", json!({})).await?;
    Ok(listed["organizations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|organization| organization["handle"] == handle)
        .cloned())
}

pub(crate) async fn organization_id(target: &Target, handle: &str) -> Result<String> {
    organization(target, handle)
        .await?
        .and_then(|organization| organization["id"].as_str().map(str::to_owned))
        .with_context(|| {
            format!("organization {handle} does not exist or is not visible to this connection")
        })
}

async fn service_users(target: &Target, organization_id: &str) -> Result<Vec<Value>> {
    let listed = call(
        target,
        "platform/organizations/service-users/list",
        json!({ "organizationId": organization_id }),
    )
    .await?;
    Ok(listed["serviceUsers"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

/// A policy with its empty parts left out, so that `{}` and
/// `{ members: [] }` compare equal.
fn normalized_policy(policy: &Value) -> Value {
    let mut parts = Map::new();
    for (key, value) in policy.as_object().into_iter().flatten() {
        if value.as_array().is_some_and(|items| !items.is_empty()) {
            parts.insert(key.clone(), value.clone());
        }
    }
    Value::Object(parts)
}

fn platform_admin(base: &ResolvedConnection) -> bool {
    base.principal
        .as_ref()
        .is_some_and(|principal| principal.platform_admin)
}

/// The steps of an organization document.
pub(crate) async fn plan_organization(
    base: &ResolvedConnection,
    target: &Target,
    subject: &str,
    document: &OrganizationDocument,
    directory: &std::path::Path,
) -> Result<Vec<Step>> {
    require_platform(base, subject)?;
    // Every key's secret is read before anything is, so a missing one fails
    // the document at once.
    let mut credentials: BTreeMap<(String, String), Value> = BTreeMap::new();
    let mut missing = Vec::new();
    for service_user in document.service_users.iter().flatten() {
        for key in service_user.keys.iter().flatten() {
            match key.secret_from.resolve(directory) {
                Ok(secret) => {
                    let mut credential = json!({ "name": key.name, "secret": secret });
                    if let Some(expires_at) = &key.expires_at {
                        credential["expiresAt"] = expires_at.clone();
                    }
                    if let Some(restrictions) = &key.restrictions {
                        credential["restrictions"] = restrictions.clone();
                    }
                    credentials.insert((service_user.name.clone(), key.name.clone()), credential);
                }
                Err(error) => missing.push(format!(
                    "serviceUsers/{}/keys/{}: {error:#}",
                    service_user.name, key.name
                )),
            }
        }
    }
    if !missing.is_empty() {
        bail!(
            "{subject}: secrets are missing, nothing was read or written:\n  {}",
            missing.join("\n  ")
        );
    }
    let handle = document.handle.as_str();
    let mut steps = Vec::new();
    let live = organization(target, handle).await?;

    let wanted = |live: Option<&Value>| {
        let mut changes = Map::new();
        let mut differs = |field: &str, value: Value, current: Option<&Value>| {
            if current != Some(&value) {
                changes.insert(field.to_owned(), value);
            }
        };
        if let Some(name) = &document.name {
            differs("name", json!(name), live.map(|live| &live["name"]));
        }
        if let Some(role) = &document.base_universe_role {
            differs(
                "baseUniverseRole",
                role.clone(),
                live.map(|live| &live["baseUniverseRole"]),
            );
        }
        if let Some(value) = document.default_protected {
            differs(
                "defaultProtected",
                json!(value),
                live.map(|live| &live["defaultProtected"]),
            );
        }
        if let Some(value) = document.require_protected {
            differs(
                "requireProtected",
                json!(value),
                live.map(|live| &live["requireProtected"]),
            );
        }
        if let Some(quota) = &document.universe_quota {
            // The view holds the limit in effect and whether it is the
            // organization's own; `null` asks for the deployment's.
            let matches = live.is_some_and(|live| {
                if quota.is_null() {
                    live["universeQuotaSource"] == "deployment"
                } else {
                    live["universeQuotaSource"] == "organization" && &live["universeQuota"] == quota
                }
            });
            if !matches {
                changes.insert("universeQuota".to_owned(), quota.clone());
            }
        }
        changes
    };

    match &live {
        None => {
            let Some(name) = &document.name else {
                steps.push(
                    Step::new(subject, "organization", handle, Action::Conflict)
                        .with_message("a new organization needs a name"),
                );
                return Ok(steps);
            };
            let mut params = wanted(None);
            params.insert("name".to_owned(), json!(name));
            params.insert("handle".to_owned(), json!(handle));
            let mut step = Step::new(subject, "organization", handle, Action::Create);
            if params.contains_key("universeQuota") && !platform_admin(base) {
                step.action = Action::Conflict;
                step.message = Some(
                    "only a Platform Admin sets a universe limit; leave universeQuota out"
                        .to_owned(),
                );
            } else {
                step.work = Some(Work::Platform(PlatformWork::CreateOrganization {
                    params: Value::Object(params),
                }));
            }
            steps.push(step);
            if let Some(policy) = &document.membership_policy {
                let mut step = Step::new(subject, "membershipPolicy", handle, Action::Create);
                step.work = Some(Work::Platform(PlatformWork::UpdateOrganization {
                    handle: handle.to_owned(),
                    changes: json!({ "membershipPolicy": policy }),
                }));
                steps.push(step);
            }
        }
        Some(live) => {
            if live["personal"] == true {
                steps.push(
                    Step::new(subject, "organization", handle, Action::Conflict)
                        .with_message("a personal organization is not configured by documents"),
                );
                return Ok(steps);
            }
            let changes = wanted(Some(live));
            let mut step = Step::new(
                subject,
                "organization",
                handle,
                if changes.is_empty() {
                    Action::Unchanged
                } else {
                    Action::Update
                },
            );
            step.fields = changes.keys().cloned().collect();
            if changes.contains_key("universeQuota") && !platform_admin(base) {
                step.action = Action::Conflict;
                step.message = Some("only a Platform Admin changes the universe limit".to_owned());
            } else if !changes.is_empty() {
                step.work = Some(Work::Platform(PlatformWork::UpdateOrganization {
                    handle: handle.to_owned(),
                    changes: Value::Object(changes),
                }));
            }
            steps.push(step);
            if let Some(policy) = &document.membership_policy {
                // Only those who manage the policy are shown it.
                let current = live.get("membershipPolicy");
                let mut step = Step::new(subject, "membershipPolicy", handle, Action::Unchanged);
                match current {
                    None => {
                        step.action = Action::Conflict;
                        step.message = Some(
                            "changing the membership policy needs an Organization Admin".to_owned(),
                        );
                    }
                    Some(current) if normalized_policy(current) != normalized_policy(policy) => {
                        step.action = Action::Update;
                        step.work = Some(Work::Platform(PlatformWork::UpdateOrganization {
                            handle: handle.to_owned(),
                            changes: json!({ "membershipPolicy": policy }),
                        }));
                    }
                    Some(_) => {}
                }
                steps.push(step);
            }
        }
    }

    if let Some(entries) = &document.service_users {
        let existing = match live.as_ref().and_then(|live| live["id"].as_str()) {
            Some(id) => service_users(target, id).await?,
            None => Vec::new(),
        };
        for entry in entries {
            let current = existing
                .iter()
                .find(|candidate| candidate["name"] == entry.name.as_str());
            let step = match current {
                None => {
                    let mut step = Step::new(subject, "serviceUsers", &entry.name, Action::Create);
                    step.work = Some(Work::Platform(PlatformWork::CreateServiceUser {
                        handle: handle.to_owned(),
                        name: entry.name.clone(),
                        role: entry.role.clone(),
                        enabled: entry.enabled,
                    }));
                    step
                }
                Some(current) => {
                    let mut changes = Map::new();
                    if current["role"] != entry.role.as_str() {
                        changes.insert("role".to_owned(), json!(entry.role));
                    }
                    if let Some(enabled) = entry.enabled
                        && current["enabled"] != enabled
                    {
                        changes.insert("enabled".to_owned(), json!(enabled));
                    }
                    let mut step = Step::new(
                        subject,
                        "serviceUsers",
                        &entry.name,
                        if changes.is_empty() {
                            Action::Unchanged
                        } else {
                            Action::Update
                        },
                    );
                    step.fields = changes.keys().cloned().collect();
                    if !changes.is_empty() {
                        step.work = Some(Work::Platform(PlatformWork::UpdateServiceUser {
                            handle: handle.to_owned(),
                            name: entry.name.clone(),
                            changes: Value::Object(changes),
                        }));
                    }
                    step
                }
            };
            steps.push(step);
            let organization_id = live.as_ref().and_then(|live| live["id"].as_str());
            for key in entry.keys.iter().flatten() {
                let credential = credentials[&(entry.name.clone(), key.name.clone())].clone();
                let id = format!("{}/{}", entry.name, key.name);
                // The server compares the secret, so a dry run says whether it would change.
                let outcome = match (
                    organization_id,
                    current.and_then(|current| current["id"].as_str()),
                ) {
                    (Some(organization_id), Some(service_user_id)) => {
                        let mut params = credential.clone();
                        params["organizationId"] = json!(organization_id);
                        params["serviceUserId"] = json!(service_user_id);
                        params["dryRun"] = json!(true);
                        Some(call(target, "platform/service-users/credentials/put", params).await?)
                    }
                    _ => None,
                };
                let mut step = match &outcome {
                    None => Step::new(subject, "keys", id, Action::Create),
                    Some(outcome) if outcome["created"] == true => {
                        Step::new(subject, "keys", id, Action::Create)
                    }
                    Some(outcome) => {
                        let fields: Vec<String> = outcome["updated"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|field| field.as_str().map(str::to_owned))
                            .collect();
                        let replaced = outcome["secretReplaced"] == true;
                        let mut step = Step::new(
                            subject,
                            "keys",
                            id,
                            if replaced || !fields.is_empty() {
                                Action::Update
                            } else {
                                Action::Unchanged
                            },
                        );
                        step.fields = fields;
                        step.secret_replaced = replaced;
                        step
                    }
                };
                if step.action != Action::Unchanged {
                    step.work = Some(Work::Platform(PlatformWork::PutServiceUserKey {
                        handle: handle.to_owned(),
                        service_user: entry.name.clone(),
                        credential,
                    }));
                }
                steps.push(step);
            }
        }
    }
    Ok(steps)
}

/// Where a universe document's universe is, or will be created.
pub(crate) enum Home {
    /// The universe exists, in this organization.
    Existing(String),
    /// It will be created in the organization with this handle, which exists
    /// or another document of the run creates.
    Handle(String),
    /// It will be created in the selected organization, by ID.
    Selected(String),
}

/// Check a universe document's `organization` against where its universe
/// is. A universe never moves between organizations.
pub(crate) async fn check_home(
    target: &Target,
    subject: &str,
    handle: Option<&str>,
    declared: &Declared,
) -> Result<(Option<Home>, Option<Step>)> {
    if let Some(universe) = target.universe_id.as_deref() {
        let read = call(
            target,
            "platform/universes/read",
            json!({ "universe": universe }),
        )
        .await?;
        let organization_id = read["universe"]["organizationId"]
            .as_str()
            .context("server did not return the universe's organization")?
            .to_owned();
        if let Some(handle) = handle {
            let expected = organization(target, handle).await?;
            if expected
                .as_ref()
                .and_then(|organization| organization["id"].as_str())
                != Some(organization_id.as_str())
            {
                let step = Step::new(subject, "universe", handle, Action::Conflict).with_message(format!(
                    "the universe is in another organization than {handle}; universes do not move between organizations"
                ));
                return Ok((Some(Home::Existing(organization_id)), Some(step)));
            }
        }
        return Ok((Some(Home::Existing(organization_id)), None));
    }
    match handle {
        Some(handle) => {
            if organization(target, handle).await?.is_none()
                && !declared.organizations.contains(handle)
            {
                bail!(
                    "{subject}: organization {handle} does not exist and no document of this run declares it"
                );
            }
            Ok((Some(Home::Handle(handle.to_owned())), None))
        }
        None => Ok((
            target
                .api
                .connection()
                .selected_organization()
                .map(|id| Home::Selected(id.to_owned())),
            None,
        )),
    }
}

/// The steps of a universe document's `access`.
pub(crate) async fn plan_access(
    target: &Target,
    subject: &str,
    universe_handle: &str,
    access: &UniverseAccess,
    home: Option<&Home>,
    declared: &Declared,
) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    let exists = target.universe_id.is_some();
    if access.members.is_some() || access.directory_group_mappings.is_some() {
        let current = match target.universe_id.as_deref() {
            Some(universe) => call(
                target,
                "platform/universes/access-policy/read",
                json!({ "universe": universe }),
            )
            .await?["policy"]
                .clone(),
            None => json!({}),
        };
        let mut policy = current.as_object().cloned().unwrap_or_default();
        if let Some(members) = &access.members {
            policy.insert("members".to_owned(), json!(members));
        }
        if let Some(mappings) = &access.directory_group_mappings {
            policy.insert("directoryGroupMappings".to_owned(), json!(mappings));
        }
        let policy = normalized_policy(&Value::Object(policy));
        let changed = normalized_policy(&current) != policy;
        let mut step = Step::new(
            subject,
            "access",
            "policy",
            match (exists, changed) {
                (_, false) => Action::Unchanged,
                (false, true) => Action::Create,
                (true, true) => Action::Update,
            },
        );
        if changed {
            step.work = Some(Work::Platform(PlatformWork::PutAccessPolicy { policy }));
        }
        steps.push(step);
    }
    let Some(grants) = &access.service_users else {
        return Ok(steps);
    };
    // The service users of the universe's organization, if it exists yet.
    let (organization_id, organization_handle) = match home {
        Some(Home::Existing(id)) | Some(Home::Selected(id)) => (Some(id.clone()), None),
        Some(Home::Handle(handle)) => (
            organization(target, handle)
                .await?
                .and_then(|organization| organization["id"].as_str().map(str::to_owned)),
            Some(handle.clone()),
        ),
        None => (None, None),
    };
    let existing = match &organization_id {
        Some(id) => service_users(target, id).await?,
        None => Vec::new(),
    };
    let live_grants = match target.universe_id.as_deref() {
        Some(universe) => call(
            target,
            "platform/universes/grants/list",
            json!({ "universe": universe }),
        )
        .await?["grants"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        None => Vec::new(),
    };
    for grant in grants {
        let id = format!("serviceUsers/{}", grant.name);
        let service_user = existing
            .iter()
            .find(|candidate| candidate["name"] == grant.name.as_str());
        let declared_here = organization_handle
            .as_ref()
            .and_then(|handle| declared.service_users.get(handle))
            .is_some_and(|names| names.contains(&grant.name));
        let current_role = service_user.and_then(|service_user| {
            live_grants
                .iter()
                .find(|live| {
                    live["principal"]["kind"] == "serviceUser"
                        && live["principal"]["id"] == service_user["id"]
                })
                .map(|live| live["role"].clone())
        });
        let mut step = match (service_user, current_role) {
            (None, _) if !declared_here => Step::new(subject, "access", id, Action::Conflict).with_message(format!(
                "no service user named {} in the organization of {universe_handle}; declare it in the organization's document",
                grant.name
            )),
            (Some(_), Some(role)) if role == grant.role.as_str() => Step::new(subject, "access", id, Action::Unchanged),
            (Some(_), Some(_)) => {
                let mut step = Step::new(subject, "access", id, Action::Update);
                step.fields = vec!["role".to_owned()];
                step
            }
            _ => Step::new(subject, "access", id, Action::Create),
        };
        if matches!(step.action, Action::Create | Action::Update) {
            step.work = Some(Work::Platform(PlatformWork::GrantServiceUser {
                name: grant.name.clone(),
                role: grant.role.clone(),
            }));
        }
        steps.push(step);
    }
    Ok(steps)
}

/// Carry out one Platform step.
pub(crate) async fn run(target: &Target, work: PlatformWork) -> Result<()> {
    match work {
        PlatformWork::CreateOrganization { params } => {
            call(target, "platform/organizations/create", params).await?;
        }
        PlatformWork::UpdateOrganization {
            handle,
            mut changes,
        } => {
            changes["organizationId"] = json!(organization_id(target, &handle).await?);
            call(target, "platform/organizations/update", changes).await?;
        }
        PlatformWork::CreateServiceUser {
            handle,
            name,
            role,
            enabled,
        } => {
            let organization_id = organization_id(target, &handle).await?;
            let created = call(
                target,
                "platform/organizations/service-users/create",
                json!({ "organizationId": organization_id, "name": name, "role": role }),
            )
            .await?;
            if enabled == Some(false) {
                call(
                    target,
                    "platform/organizations/service-users/update",
                    json!({ "organizationId": organization_id, "serviceUserId": created["serviceUser"]["id"], "enabled": false }),
                )
                .await?;
            }
        }
        PlatformWork::UpdateServiceUser {
            handle,
            name,
            mut changes,
        } => {
            let organization_id = organization_id(target, &handle).await?;
            let service_user = service_users(target, &organization_id)
                .await?
                .into_iter()
                .find(|candidate| candidate["name"] == name.as_str())
                .with_context(|| format!("service user {name} is gone"))?;
            changes["organizationId"] = json!(organization_id);
            changes["serviceUserId"] = service_user["id"].clone();
            call(
                target,
                "platform/organizations/service-users/update",
                changes,
            )
            .await?;
        }
        PlatformWork::PutServiceUserKey {
            handle,
            service_user,
            mut credential,
        } => {
            let organization_id = organization_id(target, &handle).await?;
            let found = service_users(target, &organization_id)
                .await?
                .into_iter()
                .find(|candidate| candidate["name"] == service_user.as_str())
                .with_context(|| format!("service user {service_user} is gone"))?;
            credential["organizationId"] = json!(organization_id);
            credential["serviceUserId"] = found["id"].clone();
            call(target, "platform/service-users/credentials/put", credential).await?;
        }
        PlatformWork::PutAccessPolicy { policy } => {
            let universe = target
                .universe_id
                .as_deref()
                .context("the universe does not exist")?;
            call(
                target,
                "platform/universes/access-policy/put",
                json!({ "universe": universe, "policy": policy }),
            )
            .await?;
        }
        PlatformWork::GrantServiceUser { name, role } => {
            let universe = target
                .universe_id
                .as_deref()
                .context("the universe does not exist")?;
            let read = call(
                target,
                "platform/universes/read",
                json!({ "universe": universe }),
            )
            .await?;
            let organization_id = read["universe"]["organizationId"]
                .as_str()
                .context("server did not return the universe's organization")?;
            let service_user = service_users(target, organization_id)
                .await?
                .into_iter()
                .find(|candidate| candidate["name"] == name.as_str())
                .with_context(|| {
                    format!("no service user named {name} in the universe's organization")
                })?;
            call(
                target,
                "platform/universes/grants/put",
                json!({ "universe": universe, "principal": { "kind": "serviceUser", "id": service_user["id"] }, "role": role }),
            )
            .await?;
        }
    }
    Ok(())
}

/// A universe's organization handle and its access, in normal form: the
/// policy's non-empty parts and its service users' grants by name. Access
/// is left out when the caller may not read it.
pub(crate) async fn export_universe(
    target: &Target,
) -> Result<(Option<String>, Option<UniverseAccess>)> {
    let universe = target
        .universe_id
        .as_deref()
        .context("the universe does not exist")?;
    let read = call(
        target,
        "platform/universes/read",
        json!({ "universe": universe }),
    )
    .await?;
    let organization_id = read["universe"]["organizationId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let listed = call(target, "platform/organizations/list", json!({})).await?;
    let handle = listed["organizations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|organization| organization["id"] == organization_id.as_str())
        .and_then(|organization| organization["handle"].as_str().map(str::to_owned));
    let Ok(policy) = call(
        target,
        "platform/universes/access-policy/read",
        json!({ "universe": universe }),
    )
    .await
    else {
        return Ok((handle, None));
    };
    let policy = normalized_policy(&policy["policy"]);
    let names: BTreeMap<String, String> = service_users(target, &organization_id)
        .await?
        .into_iter()
        .filter_map(|service_user| {
            Some((
                service_user["id"].as_str()?.to_owned(),
                service_user["name"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let mut grants: Vec<ServiceUserGrant> = call(
        target,
        "platform/universes/grants/list",
        json!({ "universe": universe }),
    )
    .await?["grants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|grant| grant["principal"]["kind"] == "serviceUser")
        .filter_map(|grant| {
            Some(ServiceUserGrant {
                name: names.get(grant["principal"]["id"].as_str()?)?.clone(),
                role: grant["role"].as_str()?.to_owned(),
            })
        })
        .collect();
    grants.sort_by(|a, b| a.name.cmp(&b.name));
    let list = |part: &str| policy.get(part).and_then(Value::as_array).cloned();
    let access = UniverseAccess {
        members: list("members"),
        directory_group_mappings: list("directoryGroupMappings"),
        service_users: (!grants.is_empty()).then_some(grants),
    };
    let empty = access.members.is_none()
        && access.directory_group_mappings.is_none()
        && access.service_users.is_none();
    Ok((handle, (!empty).then_some(access)))
}

/// An organization as a document, in normal form: a field left out when it
/// holds its default, service users ordered by name.
pub(crate) async fn export_organization(
    target: &Target,
    organization_id: &str,
) -> Result<OrganizationDocument> {
    let listed = call(target, "platform/organizations/list", json!({})).await?;
    let live = listed["organizations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|organization| {
            organization["id"] == organization_id || organization["handle"] == organization_id
        })
        .cloned()
        .with_context(|| {
            format!(
                "organization {organization_id} does not exist or is not visible to this connection"
            )
        })?;
    anyhow::ensure!(
        live["personal"] != true,
        "a personal organization is not configured by documents"
    );
    let policy = live
        .get("membershipPolicy")
        .map(normalized_policy)
        .filter(|policy| policy.as_object().is_some_and(|parts| !parts.is_empty()));
    let mut service_users: Vec<ServiceUserEntry> =
        service_users(target, live["id"].as_str().unwrap_or_default())
            .await?
            .into_iter()
            .filter_map(|service_user| {
                Some(ServiceUserEntry {
                    name: service_user["name"].as_str()?.to_owned(),
                    role: service_user["role"].as_str()?.to_owned(),
                    enabled: (service_user["enabled"] == false).then_some(false),
                    // A key's secret never leaves Lightspeed, so keys are
                    // declared by hand, never exported.
                    keys: None,
                })
            })
            .collect();
    service_users.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(OrganizationDocument {
        schema_version: super::document::SCHEMA_VERSION,
        kind: super::document::Kind::Organization,
        handle: live["handle"].as_str().unwrap_or_default().to_owned(),
        name: live["name"].as_str().map(str::to_owned),
        base_universe_role: Some(live["baseUniverseRole"].clone()),
        default_protected: live["defaultProtected"].as_bool(),
        require_protected: live["requireProtected"].as_bool(),
        universe_quota: (live["universeQuotaSource"] == "organization")
            .then(|| live["universeQuota"].clone()),
        membership_policy: policy,
        service_users: (!service_users.is_empty()).then_some(service_users),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_policy_parts_compare_equal_to_absent_ones() {
        assert_eq!(
            normalized_policy(&json!({ "members": [] })),
            normalized_policy(&json!({}))
        );
        assert_eq!(normalized_policy(&Value::Null), json!({}));
        assert_ne!(
            normalized_policy(
                &json!({ "directoryGroupMappings": [{ "issuer": "https://id.example", "group": "a", "role": "viewer" }] })
            ),
            normalized_policy(&json!({})),
        );
    }
}
