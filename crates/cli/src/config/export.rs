//! Writing the configuration of a live universe, or of the deployment, as a
//! document.
//!
//! The document is in normal form: entries ordered by id, server-derived
//! fields removed, optional fields left out when they hold their default,
//! and each secret named by a placeholder source. Applying it to the
//! universe it came from changes nothing.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Value, json};

use super::{
    document::{Format, Kind, SCHEMA_VERSION, placeholder_source, source_field},
    plan::{connect, deployment_resources, universe_resources},
    resources::{EnvironmentCredentials, Live, Resource, Target, Wanted},
};
use crate::connection::ResolvedConnection;

/// An entry written with its id first; the other fields follow in a stable
/// order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ordered {
    first: &'static str,
    value: Value,
}

impl Serialize for Ordered {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Some(fields) = self.value.as_object() else {
            return self.value.serialize(serializer);
        };
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        if let Some(id) = fields.get(self.first) {
            map.serialize_entry(self.first, id)?;
        }
        for (field, value) in fields {
            if field != self.first {
                map.serialize_entry(field, value)?;
            }
        }
        map.end()
    }
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exported {
    #[serde(flatten)]
    configuration: api::UniverseConfiguration,
    schema_version: u32,
    kind: Option<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    credentials: Vec<Ordered>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    providers: Vec<Ordered>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    model_defaults: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mcp_servers: Vec<Ordered>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    environment_providers: Vec<Ordered>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    registration_keys: Vec<Ordered>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    environments: Vec<Ordered>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    profiles: Vec<Ordered>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    api_keys: Vec<Ordered>,
}

impl Exported {
    fn section(&mut self, section: &str) -> Option<&mut Vec<Ordered>> {
        Some(match section {
            "credentials" => &mut self.credentials,
            "providers" => &mut self.providers,
            "mcpServers" => &mut self.mcp_servers,
            "environmentProviders" => &mut self.environment_providers,
            "registrationKeys" => &mut self.registration_keys,
            "environments" => &mut self.environments,
            "profiles" => &mut self.profiles,
            "apiKeys" => &mut self.api_keys,
            _ => return None,
        })
    }
}

/// Export the universe the connection addresses, by its UUID or handle.
pub(crate) async fn universe(base: &ResolvedConnection, universe: &str) -> Result<Exported> {
    // The handle is the universe's name in a document; the UUID never is.
    let probe = connect(base, Some(universe)).await?;
    if probe.universe_id.is_none() {
        bail!("universe {universe} does not exist");
    }
    let read = probe
        .call("deployment/universes/read", json!({ "universe": universe }))
        .await?;
    let handle = read["universe"]["handle"]
        .as_str()
        .context("the universe has no handle")?
        .to_owned();
    let target = connect(base, Some(&handle)).await?;
    let mut exported = Exported {
        schema_version: SCHEMA_VERSION,
        kind: Some(Kind::Universe),
        handle: Some(handle),
        configuration: api::UniverseConfiguration {
            display_name: read["universe"]["displayName"].as_str().map(str::to_owned),
            members: read["universe"]
                .get("members")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?,
            directory_group_mappings: read["universe"]
                .get("directoryGroupMappings")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?,
        },
        ..Exported::default()
    };
    read_sections(&target, &universe_resources(), &mut exported).await?;
    Ok(exported)
}

pub(crate) async fn deployment(base: &ResolvedConnection) -> Result<Exported> {
    let target = connect(base, None).await?;
    let mut exported = Exported {
        schema_version: SCHEMA_VERSION,
        kind: Some(Kind::Deployment),
        ..Exported::default()
    };
    read_sections(&target, &deployment_resources(), &mut exported).await?;
    Ok(exported)
}

async fn read_sections(
    target: &Target,
    resources: &[Box<dyn Resource>],
    exported: &mut Exported,
) -> Result<()> {
    let mut environment_ids = Vec::new();
    for resource in resources {
        let section = resource.section();
        if resource.owns_scope() {
            continue;
        }
        let live = resource
            .read(target, &Wanted::all())
            .await
            .with_context(|| format!("read {section}"))?;
        let entries: Vec<(String, Value)> = live
            .iter()
            .filter(|(_, live)| live.exportable)
            .map(|(id, live)| (id.clone(), export_entry(resource.as_ref(), id, live)))
            .collect();
        match section {
            "modelDefaults" => {
                for (slot, entry) in entries {
                    exported.model_defaults.insert(slot, entry["model"].clone());
                }
            }
            "environments" => {
                environment_ids = entries.iter().map(|(id, _)| id.clone()).collect();
                exported.environments = ordered(resource.as_ref(), entries);
            }
            _ => {
                if let Some(target_section) = exported.section(section) {
                    *target_section = ordered(resource.as_ref(), entries);
                }
            }
        }
    }
    if !environment_ids.is_empty() {
        attach_credentials(target, environment_ids, &mut exported.environments).await?;
    }
    Ok(())
}

/// List the credential bindings of each exported environment under it.
///
/// A binding the runtime would refuse to write is left out, so the exported
/// document can be applied: `diff` and `apply` refuse a whole document that
/// declares one.
async fn attach_credentials(
    target: &Target,
    environment_ids: Vec<String>,
    environments: &mut [Ordered],
) -> Result<()> {
    let bindings = EnvironmentCredentials
        .read(target, &Wanted::only(environment_ids))
        .await
        .context("read environment credentials")?;
    // Read only when a binding names a provider's credential.
    let provider_kinds = if bindings
        .values()
        .any(|binding| binding.entry["source"]["providerId"].is_string())
    {
        provider_kinds(target).await?
    } else {
        BTreeMap::new()
    };
    for environment in environments {
        let Some(id) = environment.value["environmentId"].as_str() else {
            continue;
        };
        let credentials: Vec<Value> = bindings
            .values()
            .filter(|binding| binding.exportable && binding.entry["environmentId"] == json!(id))
            .filter(|binding| declarable_source(&binding.entry["source"], &provider_kinds))
            .map(|binding| {
                json!({ "envName": binding.entry["envName"], "source": binding.entry["source"] })
            })
            .collect();
        if !credentials.is_empty() {
            environment.value["credentials"] = json!(credentials);
        }
    }
    Ok(())
}

/// The kind of every provider, by id.
async fn provider_kinds(target: &Target) -> Result<BTreeMap<String, String>> {
    let response = target
        .call("auth/providers/list", json!({}))
        .await
        .context("read providers")?;
    Ok(response["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|provider| {
            Some((
                provider["providerId"].as_str()?.to_owned(),
                provider["providerKind"].as_str()?.to_owned(),
            ))
        })
        .collect())
}

/// Whether a document may declare a binding with this source. The one
/// provider credential that can be bound is a model provider's API key. A
/// binding to any other provider's credential can only be left over from
/// before that rule; the runtime refuses it when it is resolved, and it is
/// state to remove, not configuration to carry into a document.
fn declarable_source(source: &Value, provider_kinds: &BTreeMap<String, String>) -> bool {
    match source["providerId"].as_str() {
        Some(provider_id) => provider_kinds
            .get(provider_id)
            .is_none_or(|kind| kind == "modelApiKey"),
        None => true,
    }
}

fn ordered(resource: &dyn Resource, entries: Vec<(String, Value)>) -> Vec<Ordered> {
    entries
        .into_iter()
        .map(|(_, value)| Ordered {
            first: resource.id_field(),
            value,
        })
        .collect()
}

/// The entry that declares a resource as it is: without the defaults, and
/// with a placeholder for where its secret comes from.
fn export_entry(resource: &dyn Resource, id: &str, live: &Live) -> Value {
    let mut entry = minimize(resource, resource.export_entry(live));
    if let Some(field) = resource.secret_field()
        && live.has_secret
        && let Some(fields) = entry.as_object_mut()
    {
        fields.insert(
            source_field(field),
            json!(placeholder_source(&format!(
                "{id}{}",
                secret_suffix(resource.section())
            ))),
        );
    }
    entry
}

/// Keeps the placeholder names of different kinds of secret apart.
fn secret_suffix(section: &str) -> &'static str {
    match section {
        "providers" => "_credential",
        "registrationKeys" => "_registration_key",
        "apiKeys" => "_api_key",
        _ => "",
    }
}

/// Drop every top-level field whose removal leaves the normal form
/// unchanged: it holds the default the server applies anyway.
fn minimize(resource: &dyn Resource, entry: Value) -> Value {
    let Some(fields) = entry.as_object() else {
        return entry;
    };
    let mut minimal = entry.clone();
    for field in fields.keys() {
        if field == resource.id_field() {
            continue;
        }
        let mut candidate = minimal.clone();
        if let Some(candidate_fields) = candidate.as_object_mut() {
            candidate_fields.remove(field);
        }
        let input_default = resource
            .normalize(candidate.clone())
            .is_ok_and(|normal| normal == entry);
        let server_default = resource.server_filled().contains(&field.as_str())
            && resource.server_default(field).as_ref() == entry.get(field);
        if input_default || server_default {
            minimal = candidate;
        }
    }
    minimal
}

/// Write each profile to a file of its own and include it from the document.
pub(crate) fn split_profiles(
    exported: &mut Exported,
    profiles_dir: &Path,
    document_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(profiles_dir)
        .with_context(|| format!("create {}", profiles_dir.display()))?;
    for profile in &mut exported.profiles {
        let Some(id) = profile.value["profileId"].as_str().map(str::to_owned) else {
            continue;
        };
        let file = profiles_dir.join(format!("{id}.json"));
        std::fs::write(&file, Format::Json.render(&*profile)?)
            .with_context(|| format!("write {}", file.display()))?;
        profile.value = json!({ "file": include_path(&file, document_dir) });
    }
    Ok(())
}

/// The path an include names: relative to the document when the file lies
/// below the document's directory, absolute otherwise.
fn include_path(file: &Path, document_dir: &Path) -> PathBuf {
    let absolute = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let file = absolute(file);
    match file.strip_prefix(absolute(document_dir)) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => file,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resources::{ApiKeys, McpServers, RegistrationKeys};

    #[test]
    fn an_exported_entry_states_only_what_differs_from_the_defaults() {
        let live = Live {
            entry: McpServers
                .normalize(json!({
                    "serverId": "tracker",
                    "serverUrl": "https://tracker.example.com/mcp",
                    "defaultServerLabel": "tracker",
                    "allowPrivateNetwork": true,
                }))
                .unwrap(),
            exportable: true,
            ..Live::default()
        };
        assert_eq!(
            export_entry(&McpServers, "tracker", &live),
            json!({
                "serverId": "tracker",
                "serverUrl": "https://tracker.example.com/mcp",
                "defaultServerLabel": "tracker",
                "allowPrivateNetwork": true,
            })
        );
    }

    #[test]
    fn an_exported_secret_is_a_placeholder_source_named_after_the_resource() {
        let live = Live {
            entry: RegistrationKeys
                .normalize(json!({
                    "registrationKeyId": "ci-runners",
                    "displayName": "CI runners",
                    "identityMode": "persistent",
                }))
                .unwrap(),
            has_secret: true,
            exportable: true,
            ..Live::default()
        };
        let mut live = live;
        // What the server filled in is left out again; a value someone
        // chose is kept.
        live.entry["ephemeralDisconnectGraceMs"] = json!(300_000);
        let entry = export_entry(&RegistrationKeys, "ci-runners", &live);
        assert!(entry.get("ephemeralDisconnectGraceMs").is_none());
        live.entry["ephemeralDisconnectGraceMs"] = json!(60_000);
        assert_eq!(
            export_entry(&RegistrationKeys, "ci-runners", &live)["ephemeralDisconnectGraceMs"],
            60_000
        );
        assert_eq!(
            entry["secretFrom"],
            json!({ "env": "CI_RUNNERS_REGISTRATION_KEY" })
        );
        assert!(entry.get("secret").is_none());
    }

    #[test]
    fn an_exported_key_lists_the_groups_it_holds() {
        // Every group a universe key may hold today. The export still names
        // them one by one: `all` is a choice someone writes, because applying
        // it after an upgrade gives the key the groups added since.
        let held: Vec<api::MethodGroup> =
            api::MethodGroup::allowed_in(api::AccessScope::Universe {
                universe_id: uuid::Uuid::nil(),
            })
            .into_iter()
            .collect();
        let live = Live {
            entry: json!({
                "keyId": "release-bot",
                "displayName": "Release bot",
                "groups": held,
                "assertActor": false,
            }),
            has_secret: true,
            exportable: true,
            ..Live::default()
        };
        let entry = export_entry(&ApiKeys, "release-bot", &live);
        assert_eq!(
            entry,
            json!({
                "keyId": "release-bot",
                "displayName": "Release bot",
                "groups": held,
                "secretFrom": { "env": "RELEASE_BOT_API_KEY" },
            })
        );
        // The exported entry declares the key as it is: nothing to change.
        let mut declared = entry.clone();
        declared.as_object_mut().unwrap().remove("secretFrom");
        let declared = ApiKeys.normalize(declared).unwrap();
        assert!(crate::config::plan::changed_fields(&ApiKeys, &declared, &live.entry).is_empty());
    }

    #[test]
    fn a_binding_the_runtime_would_refuse_is_not_exported() {
        let kinds = BTreeMap::from([
            ("model:openai".to_owned(), "modelApiKey".to_owned()),
            ("github:app".to_owned(), "gitHubApp".to_owned()),
            ("model:router".to_owned(), "modelEndpoint".to_owned()),
        ]);
        let provider = |id: &str| json!({ "type": "authProviderCredential", "providerId": id });

        assert!(declarable_source(&provider("model:openai"), &kinds));
        // Left over from before only a model provider's key could be bound.
        assert!(!declarable_source(&provider("github:app"), &kinds));
        assert!(!declarable_source(&provider("model:router"), &kinds));
        // A provider the list does not name is not judged here.
        assert!(declarable_source(&provider("unknown"), &kinds));
        // A credential is bound by id, whatever provider issued it.
        assert!(declarable_source(
            &json!({ "type": "authGrant", "grantId": "repo-token" }),
            &kinds
        ));
    }

    #[test]
    fn an_entry_is_written_with_its_id_first() {
        let entry = Ordered {
            first: "serverId",
            value: json!({ "allowPrivateNetwork": true, "serverId": "tracker", "serverUrl": "u" }),
        };
        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            r#"{"serverId":"tracker","allowPrivateNetwork":true,"serverUrl":"u"}"#
        );
        assert!(
            serde_yaml::to_string(&entry)
                .unwrap()
                .starts_with("serverId: tracker\n")
        );
    }

    #[test]
    fn profiles_can_be_written_to_files_and_included() {
        let directory = tempfile::tempdir().unwrap();
        let mut exported = Exported {
            profiles: vec![Ordered {
                first: "profileId",
                value: json!({ "profileId": "reviewer", "displayName": "Reviewer" }),
            }],
            ..Exported::default()
        };
        split_profiles(
            &mut exported,
            &directory.path().join("profiles"),
            directory.path(),
        )
        .unwrap();
        assert_eq!(
            exported.profiles[0].value,
            json!({ "file": "profiles/reviewer.json" })
        );
        let written: Value = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join("profiles/reviewer.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["displayName"], "Reviewer");
    }
}
