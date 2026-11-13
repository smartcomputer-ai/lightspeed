//! The JSON Schema of a configuration document, generated from the same
//! input types `apply` checks entries against.
//!
//! An entry's schema is the schema of the API method's input with the
//! differences a document has: the secret is replaced by its source, and the
//! fields a document never holds are removed. Unknown fields are refused at
//! every level, as the commands refuse them.
//!
//! The committed copy is `crates/cli/contract/configuration-document.schema.json`;
//! regenerate it with
//! `cargo run -q -p cli -- config schema > crates/cli/contract/configuration-document.schema.json`.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use super::{
    document::{SCHEMA_VERSION, SecretSource, source_field},
    plan::{deployment_resources, universe_resources},
    resources::{MODEL_DEFAULT_SLOTS, Resource, schema_of},
};

const DEFINITIONS: &str = "#/definitions/";

fn reference(name: &str) -> Value {
    json!({ "$ref": format!("{DEFINITIONS}{name}") })
}

/// The definition name of a section's entry: `mcpServers` has
/// `McpServersEntry`.
fn entry_name(section: &str) -> String {
    let mut name: String = section
        .split('.')
        .map(|part| {
            let mut characters = part.chars();
            characters
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
                .unwrap_or_default()
        })
        .collect();
    name.push_str("Entry");
    name
}

/// The schema of one entry: the input's schema as a document states it.
fn entry_schema(resource: &dyn Resource, generator: &mut schemars::SchemaGenerator) -> Value {
    let mut schema = resource.input_schema(generator);
    let mut hidden: Vec<String> = resource
        .undeclared_fields()
        .iter()
        .map(|field| (*field).to_owned())
        .collect();
    hidden.extend(resource.secret_field().map(str::to_owned));
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for field in &hidden {
            properties.remove(field);
        }
        if let Some(field) = resource.secret_field() {
            let mut source = reference("SecretSource");
            source["description"] = json!(format!(
                "Where `{field}` comes from. The value itself is never written in a document."
            ));
            properties.insert(source_field(field), source);
        }
    }
    let mut required: Vec<Value> = schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|field| !hidden.iter().any(|hidden| field == hidden))
        .collect();
    if let Some(field) = resource.secret_field()
        && resource.secret_required()
    {
        required.push(json!(source_field(field)));
    }
    schema["required"] = json!(required);
    schema
}

/// Refuse unknown fields wherever a schema lists the fields of an object.
fn close(schema: &mut Value) {
    match schema {
        Value::Object(fields) => {
            let lists_fields = fields.contains_key("properties");
            let combined = ["allOf", "anyOf", "oneOf"]
                .iter()
                .any(|keyword| fields.contains_key(*keyword));
            if lists_fields && !combined && !fields.contains_key("additionalProperties") {
                fields.insert("additionalProperties".to_owned(), json!(false));
            }
            for value in fields.values_mut() {
                close(value);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(close),
        _ => {}
    }
}

/// A list section: each item is an entry, or names a file that holds one.
fn list_of(section: &str) -> Value {
    json!({
        "type": "array",
        "items": { "anyOf": [reference(&entry_name(section)), reference("Include")] },
    })
}

/// The JSON Schema (draft-07) of a configuration document.
pub(crate) fn document_schema() -> Value {
    let mut generator = SchemaSettings::draft07().into_generator();
    let secret_source = schema_of::<SecretSource>(&mut generator);
    let mut entries = Map::new();
    let mut universe_sections = Map::new();
    for resource in universe_resources() {
        let section = resource.section();
        let entry = entry_schema(resource.as_ref(), &mut generator);
        match section {
            "modelDefaults" => {
                entries.insert("ModelDefault".to_owned(), entry);
                let slots: Map<String, Value> = MODEL_DEFAULT_SLOTS
                    .iter()
                    .map(|slot| {
                        (
                            (*slot).to_owned(),
                            json!({ "anyOf": [reference("ModelDefault"), { "type": "null" }] }),
                        )
                    })
                    .collect();
                universe_sections.insert(
                    section.to_owned(),
                    json!({
                        "description": "The model of each slot. `null` clears a slot; a slot left out is not managed.",
                        "type": "object",
                        "properties": slots,
                        "additionalProperties": false,
                    }),
                );
            }
            // Bindings are listed under their environment, not in a
            // section of their own.
            "environments.credentials" => {
                entries.insert(entry_name(section), entry);
            }
            _ => {
                entries.insert(entry_name(section), entry);
                universe_sections.insert(section.to_owned(), list_of(section));
            }
        }
    }
    if let Some(properties) = entries
        .get_mut(&entry_name("environments"))
        .and_then(|entry| entry.get_mut("properties"))
        .and_then(Value::as_object_mut)
    {
        properties.insert(
            "credentials".to_owned(),
            json!({
                "description": "The environment's credential bindings. When present, a binding that is not listed is unbound.",
                "type": "array",
                "items": reference(&entry_name("environments.credentials")),
            }),
        );
    }
    let deployment_sections: Map<String, Value> = deployment_resources()
        .iter()
        .map(|resource| (resource.section().to_owned(), list_of(resource.section())))
        .collect();

    let configuration = schema_of::<api::UniverseConfiguration>(&mut generator);
    if let Some(properties) = configuration.get("properties").and_then(Value::as_object) {
        universe_sections.extend(properties.clone());
    }
    let envelope = |kind: &str,
                    description: &str,
                    required: &[&str],
                    sections: Map<String, Value>| {
        let mut properties = Map::new();
        properties.insert(
            "schemaVersion".to_owned(),
            json!({
                "description": "The layout of this file. It is not a revision of the configuration.",
                "const": SCHEMA_VERSION,
            }),
        );
        properties.insert("kind".to_owned(), json!({ "const": kind }));
        if kind == "universe" {
            properties.insert(
                "handle".to_owned(),
                json!({
                    "description": "The universe's deployment-unique name. apply finds the universe by it and creates the universe when no universe has it.",
                    "type": "string",
                    "minLength": 1,
                }),
            );
        }
        properties.extend(sections);
        json!({
            "description": description,
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    };

    let mut definitions: Map<String, Value> =
        generator.take_definitions(true).into_iter().collect();
    definitions.insert("SecretSource".to_owned(), secret_source);
    definitions.insert(
        "Include".to_owned(),
        json!({
            "description": "An entry kept in a file of its own, in JSON or YAML. The path is relative to this document.",
            "type": "object",
            "properties": { "file": { "type": "string", "minLength": 1 } },
            "required": ["file"],
            "additionalProperties": false,
        }),
    );
    definitions.extend(entries);
    definitions.insert(
        "UniverseDocument".to_owned(),
        envelope(
            "universe",
            "The configuration of one universe. A section left out is not managed by the document.",
            &["schemaVersion", "kind", "handle"],
            universe_sections,
        ),
    );
    definitions.insert(
        "DeploymentDocument".to_owned(),
        envelope(
            "deployment",
            "Resources that belong to the deployment and not to one universe.",
            &["schemaVersion", "kind"],
            deployment_sections,
        ),
    );
    let mut definitions = Value::Object(definitions);
    close(&mut definitions);
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Lightspeed configuration document",
        "description": "A document that `lightspeed config apply` makes a universe, or the deployment, match. Generated from the API's input types; do not edit.",
        "oneOf": [reference("UniverseDocument"), reference("DeploymentDocument")],
        "definitions": definitions,
    })
}

/// The schema as the committed file holds it.
pub(crate) fn rendered() -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&document_schema()).expect("a schema is JSON")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document that uses every section, as the guide writes it.
    fn full_document() -> Value {
        serde_yaml::from_str(
            r#"
schemaVersion: 1
kind: universe
handle: research
displayName: Research
members:
  - { issuer: https://identity.example.com, subject: owner, role: admin }
directoryGroupMappings:
  - { issuer: https://identity.example.com, group: readers, role: viewer }
credentials:
  - grantId: tracker-token
    displayName: Issue tracker
    exposure: retrievable
    tokenFrom: { env: TRACKER_TOKEN }
providers:
  - providerId: model:primary
    config:
      type: modelApiKey
      endpoint: { baseUrl: "https://models.example.com/v1", apiKinds: ["openai:completions"] }
    credentialFrom: { file: /var/run/secrets/models/api-key }
  - providerId: model:local
    config:
      type: modelEndpoint
      endpoint:
        baseUrl: "http://127.0.0.1:11434/v1"
        apiKinds: ["openai:completions"]
        headers:
          x-title: { type: literal, value: Lightspeed }
          x-api-key: { type: credential, grantId: tracker-token }
modelDefaults:
  agentRun: { providerId: primary, apiKind: "openai:completions", model: large-1 }
  speechToText: null
mcpServers:
  - serverId: tracker
    serverUrl: https://tracker.example.com/mcp
    defaultServerLabel: tracker
    headers:
      x-upstream-token: { type: credential, grantId: tracker-token }
environmentProviders:
  - providerId: pods
    enabled: true
registrationKeys:
  - registrationKeyId: runners
    displayName: Runners
    identityMode: persistent
    declaredOnly: true
    secretFrom: { env: RUNNERS_REGISTRATION_KEY }
environments:
  - environmentId: build-box
    displayName: Build box
    metadata: { team: build }
    source: { type: registered, registrationKeyId: runners, name: lightspeed-runner-0 }
    credentials:
      - envName: TRACKER_TOKEN
        source: { type: authGrant, grantId: tracker-token }
  - environmentId: sandbox
    source:
      type: provisioned
      providerId: pods
      templateId: default
      idle: { stopAfterMs: 1800000 }
  - environmentId: lab
    source: { type: external, endpoint: "wss://lab.example.com/envd" }
profiles:
  - profileId: reviewer
    displayName: Reviewer
    instructions: { type: text, text: "Review the change and report risks." }
    config:
      features:
        environments:
          version: 1
          environments:
            - { environmentId: build-box, default: true, access: read }
  - file: ../profiles/triage.json
apiKeys:
  - keyId: release-bot
    displayName: Release bot
    groups: [session, profiles]
    secretFrom: { env: RELEASE_BOT_KEY }
"#,
        )
        .unwrap()
    }

    fn errors(document: &Value) -> Vec<String> {
        let schema = document_schema();
        let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
        validator
            .iter_errors(document)
            .map(|error| format!("{}: {error}", error.instance_path()))
            .collect()
    }

    /// Change one place of the full document.
    fn edited(edit: impl FnOnce(&mut Value)) -> Value {
        let mut document = full_document();
        edit(&mut document);
        document
    }

    #[test]
    fn the_committed_schema_is_current() {
        assert_eq!(
            include_str!("../../contract/configuration-document.schema.json"),
            rendered(),
            "crates/cli/contract/configuration-document.schema.json is stale; run `cargo run -q -p cli -- config schema > crates/cli/contract/configuration-document.schema.json` and commit the result"
        );
    }

    #[test]
    fn a_document_that_uses_every_section_is_valid() {
        assert_eq!(errors(&full_document()), Vec::<String>::new());
        let deployment = json!({
            "schemaVersion": 1,
            "kind": "deployment",
            "apiKeys": [{
                "keyId": "platform", "displayName": "Platform", "assertActor": true,
                "groups": "all",
                "secretFrom": { "env": "PLATFORM_API_KEY" },
            }],
        });
        assert_eq!(errors(&deployment), Vec::<String>::new());
    }

    #[test]
    fn a_key_states_its_groups_as_a_list_or_all() {
        let schema = document_schema();
        let key = &schema["definitions"]["ApiKeysEntry"];
        assert!(
            key["required"]
                .as_array()
                .unwrap()
                .contains(&json!("groups"))
        );
        let with_groups =
            |groups: Value| edited(|document| document["apiKeys"][0]["groups"] = groups);
        for valid in [json!("all"), json!(["session"])] {
            let document = with_groups(valid.clone());
            assert_eq!(errors(&document), Vec::<String>::new(), "{valid}");
            assert!(accepted_by_the_commands(document), "{valid}");
        }
        let unstated = edited(|document| {
            document["apiKeys"][0]
                .as_object_mut()
                .unwrap()
                .remove("groups");
        });
        for (what, document) in [
            ("a key without groups", unstated),
            ("another word than all", with_groups(json!("everything"))),
            ("an unknown group", with_groups(json!(["session", "nope"]))),
            ("groups that are neither", with_groups(json!(true))),
        ] {
            assert!(!errors(&document).is_empty(), "the schema accepts {what}");
            assert!(
                !accepted_by_the_commands(document),
                "the commands accept {what}"
            );
        }
    }

    #[test]
    fn what_the_commands_refuse_is_invalid() {
        let cases: Vec<(&str, Value)> = vec![
            (
                "an unknown section",
                edited(|document| document["profile"] = json!([])),
            ),
            (
                "an unknown field of an entry",
                edited(|document| document["mcpServers"][0]["serverURL"] = json!("typo")),
            ),
            (
                "an unknown field below the top level of an entry",
                edited(|document| {
                    document["environments"][0]["source"]["daemonId"] = json!("daemon_1")
                }),
            ),
            (
                "a secret written in the document",
                edited(|document| document["credentials"][0]["token"] = json!("plaintext")),
            ),
            (
                "a secret without a source",
                edited(|document| {
                    document["registrationKeys"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("secretFrom");
                }),
            ),
            (
                "a source that names two places",
                edited(|document| {
                    document["credentials"][0]["tokenFrom"] = json!({ "env": "A", "file": "b" })
                }),
            ),
            (
                "a field apply supplies",
                edited(|document| document["mcpServers"][0]["expectedRevision"] = json!(3)),
            ),
            (
                // A header always says where its value comes from.
                "an endpoint header written as a bare string",
                edited(|document| {
                    document["providers"][1]["config"]["endpoint"]["headers"]["x-title"] =
                        json!("Lightspeed")
                }),
            ),
            (
                "a scope on a key",
                edited(|document| {
                    document["apiKeys"][0]["scope"] = json!({ "kind": "deployment" })
                }),
            ),
            (
                "the environment repeated in its credential",
                edited(|document| {
                    document["environments"][0]["credentials"][0]["environmentId"] =
                        json!("build-box")
                }),
            ),
            (
                "an unknown model slot",
                edited(|document| document["modelDefaults"]["chat"] = json!(null)),
            ),
            (
                "another schema version",
                edited(|document| document["schemaVersion"] = json!(2)),
            ),
            (
                "a universe section in a deployment document",
                json!({ "schemaVersion": 1, "kind": "deployment", "profiles": [] }),
            ),
            (
                "a universe without a handle",
                json!({ "schemaVersion": 1, "kind": "universe" }),
            ),
        ];
        for (what, document) in cases {
            assert!(!errors(&document).is_empty(), "{what} must be invalid");
        }
    }

    /// The schema and the commands must agree, or an editor would accept
    /// what `apply` refuses. Includes are resolved when a file is loaded,
    /// before entries are checked, so the included profile is left out here.
    fn accepted_by_the_commands(mut document: Value) -> bool {
        if let Some(profiles) = document["profiles"].as_array_mut() {
            profiles.retain(|profile| profile.get("file").is_none());
        }
        serde_json::from_value::<crate::config::document::UniverseDocument>(document)
            .map_err(anyhow::Error::from)
            .and_then(|document| crate::config::plan::check_universe(&document))
            .is_ok()
    }

    #[test]
    fn the_schema_and_the_commands_agree() {
        assert!(accepted_by_the_commands(full_document()));
        let mistakes: Vec<(&str, Value)> = vec![
            (
                "an unknown section",
                edited(|document| document["profile"] = json!([])),
            ),
            (
                "an unknown field of an entry",
                edited(|document| document["providers"][0]["displayNam"] = json!("typo")),
            ),
            (
                // The name people see belongs to the Platform organization,
                // which no document reaches.
                "a display name for the universe",
                edited(|document| document["name"] = json!("Research")),
            ),
            (
                "an unknown field deep in a profile",
                edited(|document| {
                    document["profiles"][0]["config"]["features"]["environments"]["environments"]
                        [0]["defualt"] = json!(true)
                }),
            ),
            (
                "an unknown field in a registered source",
                edited(|document| document["environments"][0]["source"]["daemonId"] = json!("d")),
            ),
            (
                "a secret written in the document",
                edited(|document| document["apiKeys"][0]["secret"] = json!("lsk_plaintext")),
            ),
            (
                "a provisioned source without a template",
                edited(|document| {
                    document["environments"][1]["source"]
                        .as_object_mut()
                        .unwrap()
                        .remove("templateId");
                }),
            ),
            (
                "a wrong type",
                edited(|document| document["registrationKeys"][0]["declaredOnly"] = json!("yes")),
            ),
        ];
        for (what, document) in mistakes {
            assert!(!errors(&document).is_empty(), "the schema accepts {what}");
            assert!(
                !accepted_by_the_commands(document),
                "the commands accept {what}"
            );
        }
    }

    #[test]
    fn an_optional_provider_credential_stays_optional() {
        // The second provider of the full document has no credential.
        let schema = document_schema();
        let provider = &schema["definitions"]["ProvidersEntry"];
        assert!(provider["properties"].get("credentialFrom").is_some());
        assert!(provider["properties"].get("credential").is_none());
        assert!(
            !provider["required"]
                .as_array()
                .unwrap()
                .contains(&json!("credentialFrom"))
        );
        let credential = &schema["definitions"]["CredentialsEntry"];
        assert!(
            credential["required"]
                .as_array()
                .unwrap()
                .contains(&json!("tokenFrom"))
        );
        assert!(credential["properties"].get("dryRun").is_none());
    }
}
