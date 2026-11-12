//! Planning and applying a document.
//!
//! Four phases, and nothing is written before the last one:
//!
//! 1. load: check every entry against its input type and the references
//!    between entries;
//! 2. read: resolve secret sources, find the universe, read the current
//!    state of the sections the document contains;
//! 3. plan: compare each entry with the current state;
//! 4. write: call the write method of each entry that differs, in
//!    dependency order.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    document::{Document, LoadedDocument, SecretSource, source_field},
    resources::{
        ApiKeys, Credentials, EnvironmentCredentials, EnvironmentProviders, Environments, Live,
        McpServers, ModelDefaults, Profiles, Providers, RegistrationKeys, Resource, Target, Wanted,
    },
};
use crate::{api_client::HttpAgentApi, connection::ResolvedConnection};

/// The sections of a universe document, in the order they are written:
/// what is referenced comes before what references it.
pub(crate) fn universe_resources() -> Vec<Box<dyn Resource>> {
    vec![
        Box::new(Credentials),
        Box::new(Providers),
        Box::new(ModelDefaults),
        Box::new(McpServers),
        Box::new(EnvironmentProviders),
        Box::new(RegistrationKeys),
        Box::new(Environments),
        Box::new(EnvironmentCredentials),
        Box::new(Profiles),
        Box::new(ApiKeys),
    ]
}

pub(crate) fn deployment_resources() -> Vec<Box<dyn Resource>> {
    vec![Box::new(ApiKeys)]
}

/// One checked entry of a document.
struct Entry {
    id: String,
    body: Value,
    secret: Option<SecretSource>,
}

/// What `apply` does, or would do, to one resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Action {
    Create,
    Update,
    Remove,
    Unchanged,
    /// The document asks for something `apply` will not do.
    Conflict,
    /// The write was attempted and refused.
    Failed,
}

impl Action {
    fn changes(self) -> bool {
        matches!(self, Self::Create | Self::Update | Self::Remove)
    }

    /// The word for a planned step, and for one that was carried out.
    fn label(self, done: bool) -> &'static str {
        match (self, done) {
            (Self::Create, false) => "create",
            (Self::Create, true) => "created",
            (Self::Update, false) => "update",
            (Self::Update, true) => "updated",
            (Self::Remove, false) => "remove",
            (Self::Remove, true) => "removed",
            (Self::Unchanged, _) => "unchanged",
            (Self::Conflict, _) => "conflict",
            (Self::Failed, _) => "failed",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Step {
    /// `universe <handle>` or `deployment`.
    pub subject: String,
    pub section: &'static str,
    pub id: String,
    pub action: Action,
    /// The fields that differ, for an update.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// The stored secret differs from the supplied one.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub secret_replaced: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Set once `apply` carried the step out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
    #[serde(skip)]
    work: Option<Work>,
}

/// What a step needs to be carried out.
#[derive(Clone, Debug)]
enum Work {
    CreateUniverse {
        handle: String,
    },
    Write {
        resource: usize,
        entry: Value,
        secret: Option<String>,
        live: Option<Live>,
    },
    Remove {
        resource: usize,
        live: Live,
    },
}

impl Step {
    fn new(subject: &str, section: &'static str, id: impl Into<String>, action: Action) -> Self {
        Self {
            subject: subject.to_owned(),
            section,
            id: id.into(),
            action,
            fields: Vec::new(),
            secret_replaced: false,
            message: None,
            done: false,
            work: None,
        }
    }

    fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// One line for people: `update  mcpServers/tracker (serverUrl, secret)`.
    pub(crate) fn line(&self) -> String {
        let mut details = self.fields.clone();
        if self.secret_replaced {
            details.push("secret".to_owned());
        }
        let mut line = format!(
            "  {:<11} {}/{}",
            self.action.label(self.done),
            self.section,
            self.id
        );
        if !details.is_empty() && self.action == Action::Update {
            line.push_str(&format!(" ({})", details.join(", ")));
        }
        if let Some(message) = &self.message {
            line.push_str(&format!(": {message}"));
        }
        line
    }
}

/// The plan for one document and what is needed to carry it out.
pub(crate) struct DocumentPlan {
    pub subject: String,
    pub steps: Vec<Step>,
    target: Target,
    resources: Vec<Box<dyn Resource>>,
}

impl DocumentPlan {
    pub(crate) fn has_changes(&self) -> bool {
        self.steps.iter().any(|step| step.action.changes())
    }

    pub(crate) fn has_conflicts(&self) -> bool {
        self.steps
            .iter()
            .any(|step| step.action == Action::Conflict)
    }
}

/// Phase 1 alone for a universe document: every entry is checked against
/// its input type. Needs no connection.
#[cfg(test)]
pub(crate) fn check_universe(document: &super::document::UniverseDocument) -> Result<()> {
    let sections = universe_sections(document)?;
    for resource in universe_resources() {
        if let Some(raw) = sections.get(resource.section()) {
            check_entries(resource.as_ref(), raw)?;
        }
    }
    Ok(())
}

/// Phases 1 to 3 for one document.
pub(crate) async fn plan(
    base: &ResolvedConnection,
    loaded: &LoadedDocument,
) -> Result<DocumentPlan> {
    let subject = loaded.subject();
    let (resources, sections, handle) = match &loaded.document {
        Document::Universe(document) => (
            universe_resources(),
            universe_sections(document)?,
            Some(document.handle.clone()),
        ),
        Document::Deployment(document) => {
            let mut sections = BTreeMap::new();
            if let Some(keys) = &document.api_keys {
                sections.insert("apiKeys", keys.clone());
            }
            (deployment_resources(), sections, None)
        }
    };

    // Phase 1: every entry is checked before anything is read or written.
    let mut entries: BTreeMap<&'static str, Vec<Entry>> = BTreeMap::new();
    for resource in &resources {
        let Some(raw) = sections.get(resource.section()) else {
            continue;
        };
        entries.insert(resource.section(), check_entries(resource.as_ref(), raw)?);
    }
    let defined: BTreeMap<&'static str, BTreeSet<String>> = entries
        .iter()
        .map(|(section, entries)| {
            (
                *section,
                entries.iter().map(|entry| entry.id.clone()).collect(),
            )
        })
        .collect();

    // Phase 2: secrets first, so a missing one fails before any call.
    let mut secrets: BTreeMap<(&'static str, String), String> = BTreeMap::new();
    let mut missing = Vec::new();
    for (section, entries) in &entries {
        for entry in entries {
            let Some(source) = &entry.secret else {
                continue;
            };
            match source.resolve(loaded.directory()) {
                Ok(value) => {
                    secrets.insert((section, entry.id.clone()), value);
                }
                Err(error) => missing.push(format!("{section}/{}: {error:#}", entry.id)),
            }
        }
    }
    if !missing.is_empty() {
        bail!(
            "{subject}: secrets are missing, nothing was read or written:\n  {}",
            missing.join("\n  ")
        );
    }

    let target = connect(base, handle.as_deref()).await?;
    let mut steps = Vec::new();
    if let Some(handle) = &handle
        && target.universe_id.is_none()
    {
        let mut step = Step::new(&subject, "universe", handle, Action::Create);
        step.work = Some(Work::CreateUniverse {
            handle: handle.clone(),
        });
        steps.push(step);
    }

    let mut state: BTreeMap<&'static str, BTreeMap<String, Live>> = BTreeMap::new();
    for resource in &resources {
        let Some(section_entries) = entries.get(resource.section()) else {
            continue;
        };
        let wanted = wanted_for(resource.as_ref(), section_entries, &sections);
        let live = resource
            .read(&target, &wanted)
            .await
            .with_context(|| format!("{subject}: read {}", resource.section()))?;
        state.insert(resource.section(), live);
    }
    check_references(
        &subject, &resources, &entries, &defined, &target, &mut state,
    )
    .await?;

    // Phase 3.
    for (index, resource) in resources.iter().enumerate() {
        let Some(section_entries) = entries.get(resource.section()) else {
            continue;
        };
        let live = &state[resource.section()];
        for entry in section_entries {
            let secret = secrets
                .get(&(resource.section(), entry.id.clone()))
                .cloned();
            steps.push(
                plan_entry(
                    &subject,
                    index,
                    resource.as_ref(),
                    &target,
                    entry,
                    secret,
                    live.get(&entry.id),
                )
                .await?,
            );
        }
        if resource.owns_scope() {
            let listed: BTreeSet<&str> = section_entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect();
            for (id, existing) in live {
                if !listed.contains(id.as_str()) {
                    let mut step = Step::new(&subject, resource.section(), id, Action::Remove);
                    step.work = Some(Work::Remove {
                        resource: index,
                        live: existing.clone(),
                    });
                    steps.push(step);
                }
            }
        }
    }
    Ok(DocumentPlan {
        subject,
        steps,
        target,
        resources,
    })
}

/// Phase 4. Stops at the first refusal; every write is safe to repeat, so
/// running `apply` again continues from there.
pub(crate) async fn apply(plan: &mut DocumentPlan) -> Result<()> {
    if plan.has_conflicts() {
        bail!(
            "{}: the document asks for changes apply will not make; nothing was written",
            plan.subject
        );
    }
    for index in 0..plan.steps.len() {
        let Some(work) = plan.steps[index].work.clone() else {
            continue;
        };
        let result = match work {
            Work::CreateUniverse { handle } => create_universe(&mut plan.target, &handle).await,
            Work::Write {
                resource,
                entry,
                secret,
                live,
            } => {
                plan.resources[resource]
                    .write(&plan.target, &entry, secret.as_deref(), live.as_ref())
                    .await
            }
            Work::Remove { resource, live } => {
                let id = plan.steps[index].id.clone();
                plan.resources[resource]
                    .remove(&plan.target, &id, &live)
                    .await
            }
        };
        match result {
            Ok(()) => plan.steps[index].done = true,
            Err(error) => {
                let step = &mut plan.steps[index];
                step.action = Action::Failed;
                step.message = Some(format!("{error:#}"));
                bail!(
                    "{}: {}/{} failed; later changes were not attempted. Run `config apply` again once it is resolved.",
                    plan.subject,
                    step.section,
                    step.id
                );
            }
        }
    }
    Ok(())
}

async fn create_universe(target: &mut Target, handle: &str) -> Result<()> {
    let response = target
        .call(
            "deployment/universes/create",
            json!({ "universeId": uuid::Uuid::new_v4().to_string(), "handle": handle }),
        )
        .await?;
    target.universe_id = response["universe"]["universeId"]
        .as_str()
        .map(str::to_owned);
    if target.universe_id.is_none() {
        bail!("the runtime did not return the created universe");
    }
    Ok(())
}

/// A client for the document's universe, addressed by its handle, and the
/// universe's UUID when it exists.
pub(crate) async fn connect(base: &ResolvedConnection, handle: Option<&str>) -> Result<Target> {
    let Some(handle) = handle else {
        return Ok(Target {
            api: HttpAgentApi::with_connection(base.clone()),
            universe_id: None,
            deployment: true,
        });
    };
    let api = HttpAgentApi::with_connection(ResolvedConnection {
        universe: Some(handle.to_owned()),
        ..base.clone()
    });
    let read = api
        .request::<Value, Value>("deployment/universes/read", json!({ "universe": handle }))
        .await;
    let universe_id = match read {
        Ok(outcome) => outcome.result["universe"]["universeId"]
            .as_str()
            .map(str::to_owned),
        Err(error) if error.kind == api::AgentApiErrorKind::NotFound => None,
        Err(error) if error.kind == api::AgentApiErrorKind::Forbidden => bail!(
            "universe {handle}: {error}. Configuration commands need a deployment key that may read universes"
        ),
        Err(error) => bail!("universe {handle}: {error}"),
    };
    Ok(Target {
        api,
        universe_id,
        deployment: false,
    })
}

/// The raw entries of each section. Model defaults become one entry per
/// slot, and the credential lists of environments one entry per binding.
fn universe_sections(
    document: &super::document::UniverseDocument,
) -> Result<BTreeMap<&'static str, Vec<Value>>> {
    let mut sections: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let mut put = |section: &'static str, entries: &Option<Vec<Value>>| {
        if let Some(entries) = entries {
            sections.insert(section, entries.clone());
        }
    };
    put("credentials", &document.credentials);
    put("providers", &document.providers);
    put("mcpServers", &document.mcp_servers);
    put("environmentProviders", &document.environment_providers);
    put("registrationKeys", &document.registration_keys);
    put("environments", &document.environments);
    put("profiles", &document.profiles);
    put("apiKeys", &document.api_keys);
    if let Some(defaults) = &document.model_defaults {
        sections.insert(
            "modelDefaults",
            defaults
                .iter()
                .map(|(slot, model)| json!({ "slot": slot, "model": model }))
                .collect(),
        );
    }
    if let Some(environments) = &document.environments {
        let mut bindings = Vec::new();
        let mut owned = false;
        for environment in environments {
            let Some(credentials) = environment.get("credentials") else {
                continue;
            };
            owned = true;
            let environment_id = environment["environmentId"].clone();
            let credentials = credentials.as_array().with_context(|| {
                format!("environments/{environment_id}: credentials must be a list")
            })?;
            for credential in credentials {
                let mut binding = credential.clone();
                let Some(fields) = binding.as_object_mut() else {
                    bail!("environments/{environment_id}: a credential must be an object");
                };
                if fields.contains_key("environmentId") {
                    bail!(
                        "environments/{environment_id}: unknown field `environmentId` in a credential; it is the environment it is listed under"
                    );
                }
                fields.insert("environmentId".to_owned(), environment_id.clone());
                bindings.push(binding);
            }
        }
        if owned {
            sections.insert("environments.credentials", bindings);
        }
    }
    Ok(sections)
}

/// Phase 1 for one section: take the secret source out of each entry, refuse
/// what a document must not hold, and normalize.
fn check_entries(resource: &dyn Resource, raw: &[Value]) -> Result<Vec<Entry>> {
    let section = resource.section();
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (position, raw_entry) in raw.iter().enumerate() {
        let mut body = raw_entry.clone();
        let label = resource
            .id_of(&body)
            .map(|id| format!("{section}/{id}"))
            .unwrap_or_else(|| format!("{section}[{position}]"));
        let Some(fields) = body.as_object_mut() else {
            bail!("{label}: an entry must be an object");
        };
        for mechanic in ["dryRun", "expectedRevision", "requestId"] {
            if fields.contains_key(mechanic) {
                bail!("{label}: unknown field `{mechanic}`; apply supplies it");
            }
        }
        let mut secret = None;
        if let Some(field) = resource.secret_field() {
            if fields.contains_key(field) {
                bail!(
                    "{label}: a document never holds a secret; name where `{field}` comes from with `{}`",
                    source_field(field)
                );
            }
            match fields.remove(&source_field(field)) {
                Some(source) => {
                    secret = Some(serde_json::from_value::<SecretSource>(source).map_err(
                        |error| {
                            anyhow!(
                                "{label}: `{}` must be {{ env: NAME }} or {{ file: PATH }}: {error}",
                                source_field(field)
                            )
                        },
                    )?);
                }
                None if resource.secret_required() => bail!(
                    "{label}: `{}` is missing; it names where the secret comes from",
                    source_field(field)
                ),
                None => {}
            }
        }
        let body = resource
            .normalize(body)
            .map_err(|error| anyhow!("{label}: {error:#}"))?;
        let id = resource
            .id_of(&body)
            .with_context(|| format!("{label}: `{}` is missing", resource.id_field()))?;
        if !seen.insert(id.clone()) {
            bail!("{section}/{id} is declared twice");
        }
        entries.push(Entry { id, body, secret });
    }
    Ok(entries)
}

/// Which resources a section's read is for.
fn wanted_for(
    resource: &dyn Resource,
    entries: &[Entry],
    sections: &BTreeMap<&'static str, Vec<Value>>,
) -> Wanted {
    if resource.owns_scope() {
        // The environments whose `credentials` list the document owns.
        return Wanted::only(
            sections
                .get("environments")
                .into_iter()
                .flatten()
                .filter(|environment| environment.get("credentials").is_some())
                .filter_map(|environment| environment["environmentId"].as_str())
                .map(str::to_owned),
        );
    }
    Wanted::only(entries.iter().map(|entry| entry.id.clone()))
}

/// The resources an entry refers to, as (section, id).
fn references(section: &str, entry: &Value) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    let mut add = |section: &'static str, id: &Value| {
        if let Some(id) = id.as_str() {
            found.push((section, id.to_owned()));
        }
    };
    match section {
        "providers" => add("credentials", &entry["config"]["grantId"]),
        "mcpServers" => {
            add("credentials", &entry["credential"]["grantId"]);
            for header in entry["headers"].as_object().into_iter().flatten() {
                add("credentials", &header.1["grantId"]);
            }
        }
        "environments" => {
            if entry["source"]["type"] == "registered" {
                add("registrationKeys", &entry["source"]["registrationKeyId"]);
            }
        }
        "environments.credentials" => {
            add("credentials", &entry["source"]["grantId"]);
            add("providers", &entry["source"]["providerId"]);
        }
        "profiles" => {
            for attachment in entry["config"]["features"]["environments"]["environments"]
                .as_array()
                .into_iter()
                .flatten()
            {
                add("environments", &attachment["environmentId"]);
            }
        }
        _ => {}
    }
    found
}

/// A reference must resolve to an entry of the document or to a resource
/// that already exists in the target.
async fn check_references(
    subject: &str,
    resources: &[Box<dyn Resource>],
    entries: &BTreeMap<&'static str, Vec<Entry>>,
    defined: &BTreeMap<&'static str, BTreeSet<String>>,
    target: &Target,
    state: &mut BTreeMap<&'static str, BTreeMap<String, Live>>,
) -> Result<()> {
    let mut dangling = Vec::new();
    for (section, section_entries) in entries {
        for entry in section_entries {
            for (referenced, id) in references(section, &entry.body) {
                if defined.get(referenced).is_some_and(|ids| ids.contains(&id)) {
                    continue;
                }
                let known = match state.get(referenced) {
                    Some(live) if live.contains_key(&id) => true,
                    _ => {
                        // The section is not in the document, or the read
                        // was limited to the document's own entries.
                        let resource = resources
                            .iter()
                            .find(|resource| resource.section() == referenced)
                            .context("referenced section has no resource")?;
                        let live = resource
                            .read(target, &Wanted::only([id.clone()]))
                            .await
                            .with_context(|| format!("{subject}: read {referenced}"))?;
                        let known = live.contains_key(&id);
                        state.entry(referenced).or_default().extend(live);
                        known
                    }
                };
                if !known {
                    dangling.push(format!(
                        "{section}/{} refers to {referenced}/{id}, which is neither in the document nor in the target",
                        entry.id
                    ));
                }
            }
        }
    }
    if !dangling.is_empty() {
        bail!(
            "{subject}: unresolved references, nothing was written:\n  {}",
            dangling.join("\n  ")
        );
    }
    Ok(())
}

/// Phase 3 for one entry.
async fn plan_entry(
    subject: &str,
    index: usize,
    resource: &dyn Resource,
    target: &Target,
    entry: &Entry,
    secret: Option<String>,
    live: Option<&Live>,
) -> Result<Step> {
    let section = resource.section();
    let write = |action: Action, live: Option<&Live>| {
        let mut step = Step::new(subject, section, &entry.id, action);
        step.work = Some(Work::Write {
            resource: index,
            entry: entry.body.clone(),
            secret: secret.clone(),
            live: live.cloned(),
        });
        step
    };
    let Some(live) = live else {
        return Ok(if resource.declares_absence(&entry.body) {
            Step::new(subject, section, &entry.id, Action::Unchanged)
        } else {
            write(Action::Create, None)
        });
    };
    if let Some(reason) = &live.blocked {
        return Ok(
            Step::new(subject, section, &entry.id, Action::Conflict).with_message(reason.clone())
        );
    }
    let fields = changed_fields(resource, &entry.body, &live.entry);
    if let Some(fixed) = fields
        .iter()
        .find(|field| resource.fixed_fields().contains(&field.as_str()))
    {
        return Ok(
            Step::new(subject, section, &entry.id, Action::Conflict).with_message(format!(
                "`{fixed}` cannot be changed once the resource exists; declare it under a new id"
            )),
        );
    }
    let secret_replaced = if resource.secret_field().is_some() {
        resource
            .secret_differs(target, &entry.body, secret.as_deref(), live)
            .await
            .with_context(|| format!("{subject}: compare the secret of {section}/{}", entry.id))?
    } else {
        false
    };
    if fields.is_empty() && !secret_replaced {
        return Ok(Step::new(subject, section, &entry.id, Action::Unchanged));
    }
    let mut step = write(Action::Update, Some(live));
    step.fields = fields;
    step.secret_replaced = secret_replaced;
    Ok(step)
}

/// The top-level fields in which an entry differs from the current state.
pub(crate) fn changed_fields(resource: &dyn Resource, entry: &Value, live: &Value) -> Vec<String> {
    let mut entry = entry.clone();
    resource.adopt(&mut entry, live);
    let present = |value: &Value, field: &str| value.get(field).filter(|v| !v.is_null()).cloned();
    let fields: BTreeSet<&String> = entry
        .as_object()
        .into_iter()
        .chain(live.as_object())
        .flat_map(|map| map.keys())
        .collect();
    fields
        .into_iter()
        .filter(|field| {
            let declared = present(&entry, field);
            // A document that leaves out a field the server fills in
            // accepts what the server chose.
            if declared.is_none() && resource.server_filled().contains(&field.as_str()) {
                return false;
            }
            declared != present(live, field)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(resource: &dyn Resource, raw: Value) -> Result<Vec<Entry>> {
        check_entries(resource, raw.as_array().unwrap())
    }

    #[test]
    fn a_secret_is_named_by_its_source_and_never_written_in_the_document() {
        let checked = entries(
            &Credentials,
            json!([{ "grantId": "tracker-token", "tokenFrom": { "env": "TRACKER_TOKEN" } }]),
        )
        .unwrap();
        assert_eq!(checked[0].id, "tracker-token");
        assert_eq!(
            checked[0].secret,
            Some(SecretSource::Env("TRACKER_TOKEN".to_owned()))
        );
        assert!(checked[0].body.get("tokenFrom").is_none());
        assert!(checked[0].body.get("token").is_none());

        for (raw, expected) in [
            (
                json!([{ "grantId": "t", "token": "plaintext" }]),
                "never holds a secret",
            ),
            (json!([{ "grantId": "t" }]), "`tokenFrom` is missing"),
            (
                json!([{ "grantId": "t", "tokenFrom": { "vault": "x" } }]),
                "must be { env: NAME } or { file: PATH }",
            ),
            (
                json!([{ "grantId": "t", "tokenFrom": { "env": "T" }, "dryRun": true }]),
                "unknown field `dryRun`",
            ),
            (
                json!([
                    { "grantId": "t", "tokenFrom": { "env": "T" } },
                    { "grantId": "t", "tokenFrom": { "env": "T" } }
                ]),
                "declared twice",
            ),
        ] {
            let error = format!("{:#}", entries(&Credentials, raw).err().expect(expected));
            assert!(error.contains(expected), "{error}");
        }
        // A provider's credential is optional.
        let provider = entries(
            &Providers,
            json!([{
                "providerId": "model:local",
                "config": { "type": "modelEndpoint", "endpoint": { "baseUrl": "http://127.0.0.1:1/v1", "apiKinds": ["openai:completions"] } },
            }]),
        )
        .unwrap();
        assert_eq!(provider[0].secret, None);
    }

    #[test]
    fn only_fields_that_differ_are_reported_and_server_choices_are_accepted() {
        let declared = RegistrationKeys
            .normalize(json!({
                "registrationKeyId": "runners",
                "displayName": "Runners",
                "identityMode": "persistent",
                "declaredOnly": true,
            }))
            .unwrap();
        let live = json!({
            "registrationKeyId": "runners",
            "displayName": "Runners",
            "identityMode": "persistent",
            "declaredOnly": true,
            // The server's default, which the document did not state.
            "ephemeralDisconnectGraceMs": 300000,
        });
        assert!(changed_fields(&RegistrationKeys, &declared, &live).is_empty());

        let edited = json!({
            "registrationKeyId": "runners",
            "displayName": "Renamed in the UI",
            "identityMode": "persistent",
            "maxActiveEnvironments": 9,
            "ephemeralDisconnectGraceMs": 300000,
        });
        assert_eq!(
            changed_fields(&RegistrationKeys, &declared, &edited),
            ["declaredOnly", "displayName", "maxActiveEnvironments"]
        );

        // A stated value is compared even when the server would fill it in.
        let stated = RegistrationKeys
            .normalize(json!({
                "registrationKeyId": "runners",
                "displayName": "Runners",
                "identityMode": "persistent",
                "declaredOnly": true,
                "ephemeralDisconnectGraceMs": 60000,
            }))
            .unwrap();
        assert_eq!(
            changed_fields(&RegistrationKeys, &stated, &live),
            ["ephemeralDisconnectGraceMs"]
        );
    }

    #[test]
    fn model_defaults_and_environment_credentials_become_entries() {
        let document: super::super::document::UniverseDocument = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "universe",
            "handle": "research",
            "modelDefaults": {
                "agentRun": { "providerId": "primary", "apiKind": "openai:completions", "model": "large-1" },
                "speechToText": null,
            },
            "environments": [
                {
                    "environmentId": "build-box",
                    "source": { "type": "registered", "registrationKeyId": "runners", "name": "runner-0" },
                    "credentials": [
                        { "envName": "TRACKER_TOKEN", "source": { "type": "authGrant", "grantId": "tracker-token" } }
                    ],
                },
                { "environmentId": "lab", "source": { "type": "external", "endpoint": "wss://lab/envd" } },
            ],
        }))
        .unwrap();
        let sections = universe_sections(&document).unwrap();
        let defaults = entries(&ModelDefaults, json!(sections["modelDefaults"])).unwrap();
        assert_eq!(defaults.len(), 2);
        assert!(ModelDefaults.declares_absence(&defaults[1].body));

        let bindings = entries(
            &EnvironmentCredentials,
            json!(sections["environments.credentials"]),
        )
        .unwrap();
        assert_eq!(bindings[0].id, "build-box/TRACKER_TOKEN");
        // Only the environment that lists credentials has its bindings
        // owned by the document.
        let environments = entries(&Environments, json!(sections["environments"])).unwrap();
        let wanted = wanted_for(&EnvironmentCredentials, &bindings, &sections);
        assert_eq!(wanted.ids, Some(BTreeSet::from(["build-box".to_owned()])));
        assert!(environments[0].body.get("credentials").is_none());

        assert_eq!(
            references("environments", &environments[0].body),
            [("registrationKeys", "runners".to_owned())]
        );
        assert_eq!(
            references("environments.credentials", &bindings[0].body),
            [("credentials", "tracker-token".to_owned())]
        );
    }

    #[test]
    fn a_step_reads_as_one_line() {
        let mut step = Step::new("universe research", "mcpServers", "tracker", Action::Update);
        step.fields = vec!["serverUrl".to_owned()];
        step.secret_replaced = true;
        assert_eq!(
            step.line(),
            "  update      mcpServers/tracker (serverUrl, secret)"
        );
        step.done = true;
        assert!(step.line().starts_with("  updated "));
        let conflict = Step::new("universe research", "apiKeys", "bot", Action::Conflict)
            .with_message("is revoked");
        assert_eq!(conflict.line(), "  conflict    apiKeys/bot: is revoked");
    }
}
