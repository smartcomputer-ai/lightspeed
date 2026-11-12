//! The resources a document can hold: for each section, how an entry is
//! checked, how the current state is read in the same shape, and which
//! method writes it.
//!
//! Entries stay JSON. An entry is checked by decoding it into the API's own
//! input type and encoding it again, which rejects unknown fields and fills
//! in the defaults the server would. The current state is brought into the
//! same form from its read view, so the two can be compared field by field.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

use crate::api_client::HttpAgentApi;

/// What a document addresses: one universe, or the deployment.
pub(crate) struct Target {
    pub api: HttpAgentApi,
    /// The universe's UUID. `None` for the deployment, and for a universe
    /// that does not exist yet.
    pub universe_id: Option<String>,
    pub deployment: bool,
}

impl Target {
    /// Whether there is anything to read: a universe that does not exist yet
    /// has no state.
    pub(crate) fn exists(&self) -> bool {
        self.deployment || self.universe_id.is_some()
    }

    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.api
            .request::<Value, Value>(method, params)
            .await
            .map(|outcome| outcome.result)
            .map_err(|error| anyhow!("{method}: {error}"))
    }

    fn universe_id(&self) -> Result<&str> {
        self.universe_id
            .as_deref()
            .context("the universe does not exist yet")
    }
}

/// A resource as it is now, in the form of a document entry.
#[derive(Clone, Debug, Default)]
pub(crate) struct Live {
    /// The entry that would declare the resource as it is, without a secret.
    pub entry: Value,
    /// The revision to guard a replacement with, where the resource has one.
    pub revision: Option<u64>,
    /// The first characters of a key's secret.
    pub key_prefix: Option<String>,
    /// Whether a secret is stored for it, so `export` names a source.
    pub has_secret: bool,
    /// Why this resource cannot be declared under its id, when it cannot.
    pub blocked: Option<String>,
    /// Whether `export` writes it. State, and resources that cannot be
    /// declared, are left out.
    pub exportable: bool,
}

/// Which resources a read is for. Sections that read one resource per call
/// look only at these.
#[derive(Clone, Debug, Default)]
pub(crate) struct Wanted {
    /// `None` reads everything, as `export` does.
    pub ids: Option<BTreeSet<String>>,
}

impl Wanted {
    pub(crate) fn all() -> Self {
        Self { ids: None }
    }

    pub(crate) fn only(ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            ids: Some(ids.into_iter().collect()),
        }
    }

    fn includes(&self, id: &str) -> bool {
        self.ids.as_ref().is_none_or(|ids| ids.contains(id))
    }
}

#[async_trait]
pub(crate) trait Resource: Send + Sync {
    /// The document section, as written in a file.
    fn section(&self) -> &'static str;
    /// The field that identifies an entry.
    fn id_field(&self) -> &'static str;
    /// The wire field that carries a secret. A document declares
    /// `<field>From` instead.
    fn secret_field(&self) -> Option<&'static str> {
        None
    }
    /// Whether an entry must name a secret source.
    fn secret_required(&self) -> bool {
        self.secret_field().is_some()
    }
    /// Settings that cannot change once the resource exists.
    fn fixed_fields(&self) -> &'static [&'static str] {
        &[]
    }
    /// Fields the server fills in when an entry leaves them out. A document
    /// that omits one accepts whatever the server chose.
    fn server_filled(&self) -> &'static [&'static str] {
        &[]
    }
    /// The value the server gives a field in `server_filled` when an entry
    /// leaves it out, where that is fixed. `export` leaves such a value out.
    fn server_default(&self, _field: &str) -> Option<Value> {
        None
    }
    /// The id of an entry, for sections whose id spans several fields.
    fn id_of(&self, entry: &Value) -> Option<String> {
        entry
            .get(self.id_field())
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
    /// Check one document entry and bring it into normal form.
    fn normalize(&self, entry: Value) -> Result<Value>;
    /// Take over values the server filled in below the top level.
    fn adopt(&self, _entry: &mut Value, _live: &Value) {}
    /// An entry that declares the absence of the resource.
    fn declares_absence(&self, _entry: &Value) -> bool {
        false
    }
    /// Whether an existing resource missing from the document is removed.
    /// Only lists that belong to a declared resource are owned like this.
    fn owns_scope(&self) -> bool {
        false
    }
    async fn read(&self, target: &Target, wanted: &Wanted) -> Result<BTreeMap<String, Live>>;
    /// Whether the stored secret differs from the supplied one.
    async fn secret_differs(
        &self,
        _target: &Target,
        _entry: &Value,
        _secret: Option<&str>,
        _live: &Live,
    ) -> Result<bool> {
        Ok(false)
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        secret: Option<&str>,
        live: Option<&Live>,
    ) -> Result<()>;
    async fn remove(&self, _target: &Target, id: &str, _live: &Live) -> Result<()> {
        bail!("{} {id} cannot be removed by apply", self.section())
    }
    /// The entry `export` writes for a resource.
    fn export_entry(&self, live: &Live) -> Value {
        live.entry.clone()
    }
    /// The JSON Schema of the input an entry is checked against. Types it
    /// refers to are collected in the generator.
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value;
    /// Fields of that input a document never holds, besides the secret.
    fn undeclared_fields(&self) -> &'static [&'static str] {
        &[]
    }
}

/// The schema of an input type itself; the types it refers to stay
/// references into the generator's definitions.
pub(crate) fn schema_of<P: schemars::JsonSchema>(
    generator: &mut schemars::SchemaGenerator,
) -> Value {
    P::json_schema(generator).to_value()
}

/// Decode an entry into the API's input type and encode it again. Unknown
/// fields are an error at every level.
fn strict<P: DeserializeOwned + Serialize>(entry: Value) -> Result<Value> {
    let mut unknown = Vec::new();
    let typed: P = serde_ignored::deserialize(entry, |path| unknown.push(path.to_string()))
        .map_err(|error| anyhow!("{error}"))?;
    match unknown.as_slice() {
        [] => Ok(serde_json::to_value(typed)?),
        [field] => bail!("unknown field `{field}`"),
        fields => bail!("unknown fields `{}`", fields.join("`, `")),
    }
}

/// Bring a read view into the input type's form. Fields the input does not
/// have are dropped.
fn from_view<P: DeserializeOwned + Serialize>(view: Value) -> Result<Value> {
    let typed: P = serde_json::from_value(view)?;
    Ok(serde_json::to_value(typed)?)
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn without(mut value: Value, fields: &[&str]) -> Value {
    if let Some(map) = value.as_object_mut() {
        for field in fields {
            map.remove(*field);
        }
    }
    value
}

fn with(mut value: Value, field: &str, content: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        map.insert(field.to_owned(), content);
    }
    value
}

fn text<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn list(value: &Value, field: &str) -> Vec<Value> {
    value
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

// --- credentials -------------------------------------------------------------

/// Token credentials (`auth/grants/put`). The runtime compares the supplied
/// token itself, so a dry run says whether it would be replaced.
pub(crate) struct Credentials;

impl Credentials {
    fn params(entry: &Value, token: Option<&str>, dry_run: bool) -> Result<Value> {
        let token = token.context("a credential needs its token")?;
        let mut params = with(entry.clone(), "token", json!(token));
        if dry_run {
            params = with(params, "dryRun", json!(true));
        }
        Ok(params)
    }
}

#[async_trait]
impl Resource for Credentials {
    fn section(&self) -> &'static str {
        "credentials"
    }
    fn id_field(&self) -> &'static str {
        "grantId"
    }
    fn secret_field(&self) -> Option<&'static str> {
        Some("token")
    }
    fn server_filled(&self) -> &'static [&'static str] {
        &["providerId"]
    }
    fn server_default(&self, field: &str) -> Option<Value> {
        // Token credentials belong to the built-in provider unless told
        // otherwise.
        (field == "providerId").then(|| json!("static"))
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        let typed = strict::<api::AuthGrantPutParams>(with(entry, "token", json!("")))?;
        Ok(without(typed, &["token", "dryRun"]))
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::AuthGrantPutParams>(generator)
    }
    fn undeclared_fields(&self) -> &'static [&'static str] {
        &["dryRun"]
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target.call("auth/grants/list", json!({})).await?;
        for view in list(&response, "grants") {
            let Some(id) = text(&view, "grantId").map(str::to_owned) else {
                continue;
            };
            let token_credential = text(&view, "providerKind") == Some("staticToken");
            let active = text(&view, "status") == Some("active");
            let entry =
                from_view::<api::AuthGrantPutParams>(with(view.clone(), "token", json!("")))
                    .map(|entry| without(entry, &["token", "dryRun"]))
                    .unwrap_or(Value::Null);
            live.insert(
                id,
                Live {
                    entry,
                    blocked: (!token_credential).then(|| {
                        "is not a token credential: it was created through a sign-in flow"
                            .to_owned()
                    }),
                    exportable: token_credential && active,
                    has_secret: true,
                    ..Live::default()
                },
            );
        }
        Ok(live)
    }
    async fn secret_differs(
        &self,
        target: &Target,
        entry: &Value,
        secret: Option<&str>,
        _live: &Live,
    ) -> Result<bool> {
        let response = target
            .call("auth/grants/put", Self::params(entry, secret, true)?)
            .await?;
        Ok(response["tokenReplaced"] == json!(true))
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        secret: Option<&str>,
        _live: Option<&Live>,
    ) -> Result<()> {
        target
            .call("auth/grants/put", Self::params(entry, secret, false)?)
            .await
            .map(drop)
    }
}

// --- providers ---------------------------------------------------------------

/// Model providers and GitHub Apps (`auth/providers/put`). A provider's
/// credential is optional; a dry run says whether it would change.
pub(crate) struct Providers;

impl Providers {
    fn params(entry: &Value, credential: Option<&str>, dry_run: bool) -> Value {
        let mut params = entry.clone();
        if let Some(credential) = credential {
            params = with(params, "credential", json!(credential));
        }
        if dry_run {
            params = with(params, "dryRun", json!(true));
        }
        params
    }
}

#[async_trait]
impl Resource for Providers {
    fn section(&self) -> &'static str {
        "providers"
    }
    fn id_field(&self) -> &'static str {
        "providerId"
    }
    fn secret_field(&self) -> Option<&'static str> {
        Some("credential")
    }
    fn secret_required(&self) -> bool {
        false
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        let typed = strict::<api::AuthProviderPutParams>(entry)?;
        Ok(without(typed, &["credential", "dryRun"]))
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::AuthProviderPutParams>(generator)
    }
    fn undeclared_fields(&self) -> &'static [&'static str] {
        &["dryRun"]
    }
    fn adopt(&self, entry: &mut Value, live: &Value) {
        // A GitHub App without a base URL gets the public one.
        let filled = &live["config"]["apiBaseUrl"];
        if let Some(config) = entry.get_mut("config").and_then(Value::as_object_mut)
            && config.get("type") == Some(&json!("githubApp"))
            && !config.contains_key("apiBaseUrl")
            && !filled.is_null()
        {
            config.insert("apiBaseUrl".to_owned(), filled.clone());
        }
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target.call("auth/providers/list", json!({})).await?;
        for view in list(&response, "providers") {
            let Some(id) = text(&view, "providerId").map(str::to_owned) else {
                continue;
            };
            // Kinds the put method cannot express are not configuration a
            // document can hold.
            let entry = from_view::<api::AuthProviderPutParams>(view.clone())
                .map(|entry| without(entry, &["credential", "dryRun"]));
            let live_entry = match entry {
                Ok(entry) => Live {
                    entry,
                    exportable: text(&view, "status") == Some("active"),
                    has_secret: view["hasCredential"] == json!(true),
                    ..Live::default()
                },
                Err(_) => Live {
                    blocked: Some("is of a kind that cannot be declared".to_owned()),
                    ..Live::default()
                },
            };
            live.insert(id, live_entry);
        }
        Ok(live)
    }
    async fn secret_differs(
        &self,
        target: &Target,
        entry: &Value,
        secret: Option<&str>,
        _live: &Live,
    ) -> Result<bool> {
        let response = target
            .call("auth/providers/put", Self::params(entry, secret, true))
            .await?;
        Ok(response["credentialReplaced"] == json!(true))
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        secret: Option<&str>,
        _live: Option<&Live>,
    ) -> Result<()> {
        target
            .call("auth/providers/put", Self::params(entry, secret, false))
            .await
            .map(drop)
    }
}

// --- model defaults ----------------------------------------------------------

/// Model defaults (`models/defaults/put`), one entry per slot. In a document
/// the section is a map of slot to model; `null` clears a slot.
pub(crate) struct ModelDefaults;

pub(crate) const MODEL_DEFAULT_SLOTS: [&str; 2] = ["agentRun", "speechToText"];

#[async_trait]
impl Resource for ModelDefaults {
    fn section(&self) -> &'static str {
        "modelDefaults"
    }
    fn id_field(&self) -> &'static str {
        "slot"
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        let typed = strict::<api::ModelDefaultsPutParams>(entry)?;
        Ok(without(typed, &["expectedRevision"]))
    }
    /// In a document the section is a map of slot to model, so this is the
    /// schema of one model.
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::ModelConfig>(generator)
    }
    fn declares_absence(&self, entry: &Value) -> bool {
        entry.get("model").is_none_or(Value::is_null)
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target.call("models/defaults/read", json!({})).await?;
        for slot in MODEL_DEFAULT_SLOTS {
            let model = &response["defaults"][slot];
            if !model.is_null() {
                live.insert(
                    slot.to_owned(),
                    Live {
                        entry: json!({ "slot": slot, "model": model }),
                        exportable: true,
                        ..Live::default()
                    },
                );
            }
        }
        Ok(live)
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        _secret: Option<&str>,
        _live: Option<&Live>,
    ) -> Result<()> {
        // Both slots share one revision, so a guard read before the first
        // write would refuse the second. The slots are independent.
        let params = json!({ "slot": entry["slot"], "model": entry.get("model") });
        target.call("models/defaults/put", params).await.map(drop)
    }
}

// --- MCP servers -------------------------------------------------------------

/// MCP server records (`mcp/servers/put`).
pub(crate) struct McpServers;

#[async_trait]
impl Resource for McpServers {
    fn section(&self) -> &'static str {
        "mcpServers"
    }
    fn id_field(&self) -> &'static str {
        "serverId"
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        strict::<api::McpServerInput>(entry)
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::McpServerInput>(generator)
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target.call("mcp/servers/list", json!({})).await?;
        for view in list(&response, "servers") {
            let Some(id) = text(&view, "serverId").map(str::to_owned) else {
                continue;
            };
            let revision = view["revision"].as_u64();
            let entry = from_view::<api::McpServerInput>(without(
                view,
                &["revision", "createdAtMs", "updatedAtMs"],
            ))
            .with_context(|| format!("MCP server {id}"))?;
            live.insert(
                id,
                Live {
                    entry,
                    revision,
                    exportable: true,
                    ..Live::default()
                },
            );
        }
        Ok(live)
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        _secret: Option<&str>,
        live: Option<&Live>,
    ) -> Result<()> {
        let mut params = json!({ "server": entry });
        if let Some(revision) = live.and_then(|live| live.revision) {
            params["expectedRevision"] = json!(revision);
        }
        target.call("mcp/servers/put", params).await.map(drop)
    }
}

// --- environment providers ---------------------------------------------------

/// Which deployment providers this universe may provision from
/// (`deployment/environment-providers/bindings/put`).
pub(crate) struct EnvironmentProviders;

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderBindingEntry {
    provider_id: String,
    enabled: bool,
}

#[async_trait]
impl Resource for EnvironmentProviders {
    fn section(&self) -> &'static str {
        "environmentProviders"
    }
    fn id_field(&self) -> &'static str {
        "providerId"
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        strict::<ProviderBindingEntry>(entry)
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<ProviderBindingEntry>(generator)
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target
            .call("environments/provider-bindings/list", json!({}))
            .await?;
        for view in list(&response, "bindings") {
            let Some(id) = text(&view, "providerId").map(str::to_owned) else {
                continue;
            };
            live.insert(
                id.clone(),
                Live {
                    entry: json!({ "providerId": id, "enabled": view["enabled"] }),
                    exportable: true,
                    ..Live::default()
                },
            );
        }
        Ok(live)
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        _secret: Option<&str>,
        _live: Option<&Live>,
    ) -> Result<()> {
        let params = json!({
            "universe": target.universe_id()?,
            "providerId": entry["providerId"],
            "enabled": entry["enabled"],
        });
        target
            .call("deployment/environment-providers/bindings/put", params)
            .await
            .map(drop)
    }
}

// --- registration keys -------------------------------------------------------

/// Environment registration keys (`environments/registration-keys/put`).
pub(crate) struct RegistrationKeys;

#[async_trait]
impl Resource for RegistrationKeys {
    fn section(&self) -> &'static str {
        "registrationKeys"
    }
    fn id_field(&self) -> &'static str {
        "registrationKeyId"
    }
    fn secret_field(&self) -> Option<&'static str> {
        Some("secret")
    }
    fn fixed_fields(&self) -> &'static [&'static str] {
        &["identityMode"]
    }
    fn server_filled(&self) -> &'static [&'static str] {
        &["ephemeralDisconnectGraceMs"]
    }
    fn server_default(&self, field: &str) -> Option<Value> {
        (field == "ephemeralDisconnectGraceMs").then(|| json!(300_000))
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        let typed =
            strict::<api::EnvironmentRegistrationKeyPutParams>(with(entry, "secret", json!("")))?;
        Ok(without(typed, &["secret"]))
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::EnvironmentRegistrationKeyPutParams>(generator)
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target
            .call("environments/registration-keys/list", json!({}))
            .await?;
        for view in list(&response, "registrationKeys") {
            let Some(id) = text(&view, "registrationKeyId").map(str::to_owned) else {
                continue;
            };
            let status = text(&view, "status").unwrap_or_default().to_owned();
            let key_prefix = text(&view, "keyPrefix").map(str::to_owned);
            let entry = from_view::<api::EnvironmentRegistrationKeyPutParams>(with(
                view,
                "secret",
                json!(""),
            ))
            .map(|entry| without(entry, &["secret"]))
            .with_context(|| format!("registration key {id}"))?;
            live.insert(
                id,
                Live {
                    entry,
                    key_prefix,
                    blocked: (status == "revoked").then(|| {
                        "is revoked; a revoked key is never restored, so declare it under a new id"
                            .to_owned()
                    }),
                    exportable: status == "active",
                    has_secret: true,
                    ..Live::default()
                },
            );
        }
        Ok(live)
    }
    async fn secret_differs(
        &self,
        _target: &Target,
        _entry: &Value,
        secret: Option<&str>,
        live: &Live,
    ) -> Result<bool> {
        Ok(prefix_differs(secret, live))
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        secret: Option<&str>,
        _live: Option<&Live>,
    ) -> Result<()> {
        let secret = secret.context("a registration key needs its secret")?;
        target
            .call(
                "environments/registration-keys/put",
                with(entry.clone(), "secret", json!(secret)),
            )
            .await
            .map(drop)
    }
}

/// A key lists the first characters of its secret, which is enough to see
/// that another secret is stored without sending the supplied one.
fn prefix_differs(secret: Option<&str>, live: &Live) -> bool {
    match (secret, live.key_prefix.as_deref()) {
        (Some(secret), Some(prefix)) => !secret.starts_with(prefix),
        _ => false,
    }
}

// --- environments ------------------------------------------------------------

/// Environments of all three kinds. An entry's `source` has the shape of the
/// `source` object the API returns, reduced to its settings, and decides
/// which method creates the environment.
pub(crate) struct Environments;

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnvironmentEntry {
    environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    source: EnvironmentSourceEntry,
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum EnvironmentSourceEntry {
    /// A fixed machine whose daemon registers under `name`.
    Registered {
        registration_key_id: String,
        name: String,
    },
    Provisioned {
        provider_id: String,
        template_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle: Option<api::EnvironmentIdlePolicyView>,
    },
    External {
        endpoint: String,
    },
}

impl Environments {
    fn create(target_entry: &Value) -> Result<(&'static str, Value)> {
        let source = &target_entry["source"];
        let mut params = object(without(target_entry.clone(), &["source"]));
        let method = match text(source, "type") {
            Some("registered") => {
                params.insert(
                    "registrationKeyId".to_owned(),
                    source["registrationKeyId"].clone(),
                );
                params.insert("name".to_owned(), source["name"].clone());
                "environments/registered/put"
            }
            Some("provisioned") => {
                params.insert("providerId".to_owned(), source["providerId"].clone());
                params.insert("templateId".to_owned(), source["templateId"].clone());
                if let Some(idle) = source.get("idle") {
                    params.insert("idle".to_owned(), idle.clone());
                }
                // A retried create converges on the same environment.
                params.insert(
                    "requestId".to_owned(),
                    target_entry["environmentId"].clone(),
                );
                "environments/create"
            }
            Some("external") => {
                params.insert("endpoint".to_owned(), source["endpoint"].clone());
                params.insert(
                    "requestId".to_owned(),
                    target_entry["environmentId"].clone(),
                );
                "environments/external/create"
            }
            other => bail!("unknown environment source type {other:?}"),
        };
        Ok((method, Value::Object(params)))
    }

    /// The settings of a `source` view, without what the runtime observed.
    fn source_entry(source: &Value) -> Option<Value> {
        match text(source, "type")? {
            "registered" => Some(json!({
                "type": "registered",
                "registrationKeyId": source["registrationKeyId"],
                "name": source.get("name")?,
            })),
            "provisioned" => {
                let mut entry = json!({
                    "type": "provisioned",
                    "providerId": source["providerId"],
                    "templateId": source["templateId"],
                });
                if let Some(idle) = source.get("idle").filter(|idle| !idle.is_null()) {
                    entry["idle"] = idle.clone();
                }
                Some(entry)
            }
            "external" => Some(json!({ "type": "external", "endpoint": source["endpoint"] })),
            _ => None,
        }
    }
}

#[async_trait]
impl Resource for Environments {
    fn section(&self) -> &'static str {
        "environments"
    }
    fn id_field(&self) -> &'static str {
        "environmentId"
    }
    fn fixed_fields(&self) -> &'static [&'static str] {
        &["source"]
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        strict::<EnvironmentEntry>(without(entry, &["credentials"]))
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<EnvironmentEntry>(generator)
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let mut after: Option<String> = None;
        loop {
            let mut params = json!({ "includeClosed": true, "limit": 500 });
            if let Some(after) = &after {
                params["after"] = json!(after);
            }
            let response = target.call("environments/list", params).await?;
            for view in list(&response, "environments") {
                let Some(id) = text(&view, "environmentId").map(str::to_owned) else {
                    continue;
                };
                let state = text(&view, "state").unwrap_or_default();
                let source = Self::source_entry(&view["source"]);
                let blocked = if state == "closed" {
                    Some(
                        "is closed; its id stays with the closed environment, so declare it under a new id"
                            .to_owned(),
                    )
                } else if source.is_none() {
                    Some("was registered by its daemon without a declaration".to_owned())
                } else {
                    None
                };
                let mut entry = json!({
                    "environmentId": id,
                    "metadata": view.get("metadata").cloned().unwrap_or_else(|| json!({})),
                    "source": source,
                });
                if let Some(name) = text(&view, "displayName") {
                    entry["displayName"] = json!(name);
                }
                live.insert(
                    id,
                    Live {
                        entry,
                        exportable: blocked.is_none() && matches!(state, "open" | "provisioning"),
                        blocked,
                        ..Live::default()
                    },
                );
            }
            after = text(&response, "nextAfter").map(str::to_owned);
            if after.is_none() {
                break;
            }
        }
        Ok(live)
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        _secret: Option<&str>,
        live: Option<&Live>,
    ) -> Result<()> {
        let registered = text(&entry["source"], "type") == Some("registered");
        // Declaring a fixed machine again converges its name and metadata;
        // the other kinds are created once and then updated.
        if live.is_none() || registered {
            let (method, params) = Self::create(entry)?;
            return target.call(method, params).await.map(drop);
        }
        target
            .call("environments/update", without(entry.clone(), &["source"]))
            .await
            .map(drop)
    }
}

/// The credential bindings of declared environments
/// (`environments/credentials/bind`). An environment entry that has a
/// `credentials` list owns its bindings: one that is not listed is unbound.
pub(crate) struct EnvironmentCredentials;

impl EnvironmentCredentials {
    pub(crate) fn id(environment_id: &str, env_name: &str) -> String {
        format!("{environment_id}/{env_name}")
    }
}

#[async_trait]
impl Resource for EnvironmentCredentials {
    fn section(&self) -> &'static str {
        "environments.credentials"
    }
    fn id_field(&self) -> &'static str {
        "envName"
    }
    fn id_of(&self, entry: &Value) -> Option<String> {
        Some(Self::id(
            text(entry, "environmentId")?,
            text(entry, "envName")?,
        ))
    }
    fn owns_scope(&self) -> bool {
        true
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        let typed = strict::<api::EnvironmentCredentialBindParams>(entry)?;
        if text(&typed["source"], "type") == Some("directSecret") {
            bail!(
                "a directly stored secret cannot be declared; bind a credential or a provider credential by id"
            );
        }
        Ok(typed)
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::EnvironmentCredentialBindParams>(generator)
    }
    /// A binding is listed under its environment.
    fn undeclared_fields(&self) -> &'static [&'static str] {
        &["environmentId"]
    }
    /// `wanted` names the environments whose bindings the document owns.
    async fn read(&self, target: &Target, wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        for environment_id in wanted.ids.iter().flatten() {
            // An environment the same apply is about to create has none.
            let Ok(response) = target
                .call(
                    "environments/credentials/list",
                    json!({ "environmentId": environment_id }),
                )
                .await
            else {
                continue;
            };
            for view in list(&response, "credentials") {
                let Some(env_name) = text(&view, "envName") else {
                    continue;
                };
                live.insert(
                    Self::id(environment_id, env_name),
                    Live {
                        entry: json!({
                            "environmentId": environment_id,
                            "envName": env_name,
                            "source": view["source"],
                        }),
                        exportable: text(&view["source"], "type") != Some("directSecret"),
                        ..Live::default()
                    },
                );
            }
        }
        Ok(live)
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        _secret: Option<&str>,
        _live: Option<&Live>,
    ) -> Result<()> {
        target
            .call("environments/credentials/bind", entry.clone())
            .await
            .map(drop)
    }
    async fn remove(&self, target: &Target, _id: &str, live: &Live) -> Result<()> {
        target
            .call(
                "environments/credentials/unbind",
                without(live.entry.clone(), &["source"]),
            )
            .await
            .map(drop)
    }
}

// --- profiles ----------------------------------------------------------------

/// Agent profiles (`profiles/put`). An entry is the profile the API calls
/// `AgentProfileInput`, which is also what `profile export` writes.
pub(crate) struct Profiles;

const PROFILE_FIELDS: [&str; 7] = [
    "profileId",
    "displayName",
    "description",
    "config",
    "instructions",
    "metadata",
    "retention",
];

impl Profiles {
    /// Instructions stored as a blob reference mean nothing in another
    /// universe, so a document carries the text.
    async fn inline_instructions(target: &Target, profile: &mut Value) -> Result<()> {
        let Some(blob_ref) = profile["instructions"]
            .get("blobRef")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(());
        };
        use base64::Engine as _;
        let blob = target
            .call("blobs/read", json!({ "blobRef": blob_ref }))
            .await?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text(&blob, "bytesBase64").unwrap_or_default())
            .context("decode profile instructions")?;
        let instructions =
            String::from_utf8(bytes).context("profile instructions are not UTF-8 text")?;
        profile["instructions"] = json!({ "type": "text", "text": instructions });
        Ok(())
    }
}

#[async_trait]
impl Resource for Profiles {
    fn section(&self) -> &'static str {
        "profiles"
    }
    fn id_field(&self) -> &'static str {
        "profileId"
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        // The profile body is flattened into the profile on the wire, which
        // hides unknown top-level fields from the decoder.
        if let Some(unknown) = entry.as_object().and_then(|map| {
            map.keys()
                .find(|key| !PROFILE_FIELDS.contains(&key.as_str()))
        }) {
            bail!("unknown field `{unknown}`");
        }
        if entry["instructions"].get("blobRef").is_some() {
            bail!(
                "instructions must be text: a blob reference belongs to one universe and is not portable"
            );
        }
        strict::<api::AgentProfileInput>(entry)
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::AgentProfileInput>(generator)
    }
    async fn read(&self, target: &Target, wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target.call("profiles/list", json!({})).await?;
        for summary in list(&response, "profiles") {
            let Some(id) = text(&summary, "profileId").map(str::to_owned) else {
                continue;
            };
            if !wanted.includes(&id) {
                continue;
            }
            let read = target
                .call("profiles/read", json!({ "profileId": id }))
                .await?;
            let mut profile = read["profile"].clone();
            let revision = profile["revision"].as_u64();
            Self::inline_instructions(target, &mut profile).await?;
            let entry = from_view::<api::AgentProfileInput>(without(
                profile,
                &["revision", "createdAtMs", "updatedAtMs"],
            ))
            .with_context(|| format!("profile {id}"))?;
            live.insert(
                id,
                Live {
                    entry,
                    revision,
                    exportable: true,
                    ..Live::default()
                },
            );
        }
        Ok(live)
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        _secret: Option<&str>,
        live: Option<&Live>,
    ) -> Result<()> {
        let mut params = json!({ "profile": entry });
        if let Some(revision) = live.and_then(|live| live.revision) {
            params["expectedRevision"] = json!(revision);
        }
        target.call("profiles/put", params).await.map(drop)
    }
}

// --- API keys ----------------------------------------------------------------

/// API keys (`deployment/api-keys/put`), scoped to the document's universe
/// or to the deployment. The scope is implied by the document.
pub(crate) struct ApiKeys;

impl ApiKeys {
    fn scope(target: &Target) -> Result<Value> {
        Ok(if target.deployment {
            json!({ "kind": "deployment" })
        } else {
            json!({ "kind": "universe", "universeId": target.universe_id()? })
        })
    }
}

#[async_trait]
impl Resource for ApiKeys {
    fn section(&self) -> &'static str {
        "apiKeys"
    }
    fn id_field(&self) -> &'static str {
        "keyId"
    }
    fn secret_field(&self) -> Option<&'static str> {
        Some("secret")
    }
    fn fixed_fields(&self) -> &'static [&'static str] {
        &["assertActor"]
    }
    fn server_filled(&self) -> &'static [&'static str] {
        &["groups"]
    }
    fn normalize(&self, entry: Value) -> Result<Value> {
        if entry.get("scope").is_some() {
            bail!("unknown field `scope`: a key's scope is the document it is declared in");
        }
        let placed = with(
            with(entry, "secret", json!("")),
            "scope",
            json!({ "kind": "deployment" }),
        );
        let typed = strict::<api::DeploymentApiKeyPutParams>(placed)?;
        Ok(without(typed, &["secret", "scope"]))
    }
    fn input_schema(&self, generator: &mut schemars::SchemaGenerator) -> Value {
        schema_of::<api::DeploymentApiKeyPutParams>(generator)
    }
    fn undeclared_fields(&self) -> &'static [&'static str] {
        &["scope"]
    }
    async fn read(&self, target: &Target, _wanted: &Wanted) -> Result<BTreeMap<String, Live>> {
        let mut live = BTreeMap::new();
        if !target.exists() {
            return Ok(live);
        }
        let response = target
            .call(
                "deployment/api-keys/list",
                json!({ "scope": Self::scope(target)? }),
            )
            .await?;
        for view in list(&response, "apiKeys") {
            let Some(id) = text(&view, "keyId").map(str::to_owned) else {
                continue;
            };
            let revoked = view.get("revokedAtMs").is_some_and(|at| !at.is_null());
            live.insert(
                id.clone(),
                Live {
                    entry: json!({
                        "keyId": id,
                        "displayName": text(&view, "displayName").unwrap_or(&id),
                        "groups": view["groups"],
                        "assertActor": view["assertActor"],
                    }),
                    key_prefix: text(&view, "keyPrefix").map(str::to_owned),
                    blocked: revoked.then(|| {
                        "is revoked; a revoked key is never restored, so declare it under a new id"
                            .to_owned()
                    }),
                    exportable: !revoked,
                    has_secret: true,
                    ..Live::default()
                },
            );
        }
        Ok(live)
    }
    async fn secret_differs(
        &self,
        _target: &Target,
        _entry: &Value,
        secret: Option<&str>,
        live: &Live,
    ) -> Result<bool> {
        Ok(prefix_differs(secret, live))
    }
    async fn write(
        &self,
        target: &Target,
        entry: &Value,
        secret: Option<&str>,
        _live: Option<&Live>,
    ) -> Result<()> {
        let secret = secret.context("an API key needs its secret")?;
        let params = with(
            with(entry.clone(), "secret", json!(secret)),
            "scope",
            Self::scope(target)?,
        );
        target
            .call("deployment/api-keys/put", params)
            .await
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_checked_against_the_input_type_and_gets_its_defaults() {
        let entry = McpServers
            .normalize(json!({
                "serverId": "tracker",
                "serverUrl": "https://tracker.example.com/mcp",
                "defaultServerLabel": "tracker",
                "headers": { "x-token": { "type": "credential", "grantId": "tracker-token" } },
            }))
            .unwrap();
        // Defaults the server would apply are part of the normal form, so
        // an entry that leaves them out equals one that states them.
        assert_eq!(entry["status"], "active");
        assert_eq!(entry["allowPrivateNetwork"], false);
        assert_eq!(entry["headers"]["x-token"]["grantId"], "tracker-token");

        let error = McpServers
            .normalize(json!({
                "serverId": "tracker",
                "serverUrl": "https://tracker.example.com/mcp",
                "defaultServerLabel": "tracker",
                "serverURL": "typo",
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("serverURL"), "{error}");
    }

    #[test]
    fn unknown_fields_are_refused_at_every_level() {
        let nested = Credentials
            .normalize(json!({ "grantId": "t", "displayNam": "typo" }))
            .unwrap_err()
            .to_string();
        assert!(nested.contains("unknown field `displayNam`"), "{nested}");

        let profile = Profiles
            .normalize(json!({ "profileId": "p", "displayNam": "typo" }))
            .unwrap_err()
            .to_string();
        assert!(profile.contains("unknown field `displayNam`"), "{profile}");

        let source = Environments
            .normalize(json!({
                "environmentId": "build-box",
                "source": { "type": "registered", "registrationKeyId": "runners", "name": "r-0", "daemonId": "x" },
            }))
            .unwrap_err()
            .to_string();
        assert!(source.contains("daemonId"), "{source}");
    }

    #[test]
    fn a_document_never_names_a_scope_or_a_stored_secret() {
        assert!(
            ApiKeys
                .normalize(
                    json!({ "keyId": "k", "displayName": "K", "scope": { "kind": "deployment" } })
                )
                .is_err()
        );
        let key = ApiKeys
            .normalize(json!({ "keyId": "k", "displayName": "K", "groups": ["session"] }))
            .unwrap();
        assert!(key.get("secret").is_none() && key.get("scope").is_none());
        assert_eq!(key["assertActor"], false);

        assert!(
            EnvironmentCredentials
                .normalize(json!({
                    "environmentId": "build-box",
                    "envName": "TOKEN",
                    "source": { "type": "directSecret", "secretId": "s" },
                }))
                .is_err()
        );
        assert!(
            Profiles
                .normalize(json!({
                    "profileId": "p",
                    "instructions": { "type": "textRef", "blobRef": "sha256:00" },
                }))
                .is_err()
        );
    }

    #[test]
    fn each_environment_kind_is_created_by_its_own_method() {
        let entry = |source: Value| {
            Environments
                .normalize(json!({ "environmentId": "e", "displayName": "E", "source": source }))
                .unwrap()
        };
        let (method, params) = Environments::create(&entry(json!({
            "type": "registered", "registrationKeyId": "runners", "name": "runner-0",
        })))
        .unwrap();
        assert_eq!(method, "environments/registered/put");
        assert_eq!(
            params,
            json!({
                "environmentId": "e",
                "displayName": "E",
                "metadata": {},
                "registrationKeyId": "runners",
                "name": "runner-0",
            })
        );

        let (method, params) = Environments::create(&entry(json!({
            "type": "provisioned", "providerId": "pods", "templateId": "default",
            "idle": { "stopAfterMs": 60000 },
        })))
        .unwrap();
        assert_eq!(method, "environments/create");
        assert_eq!(params["requestId"], "e", "a retried create converges");
        assert_eq!(params["idle"]["stopAfterMs"], 60000);

        let (method, params) = Environments::create(&entry(
            json!({ "type": "external", "endpoint": "wss://e/envd" }),
        ))
        .unwrap();
        assert_eq!(method, "environments/external/create");
        assert_eq!(params["endpoint"], "wss://e/envd");
    }

    #[test]
    fn a_source_view_is_reduced_to_its_settings() {
        assert_eq!(
            Environments::source_entry(&json!({
                "type": "registered", "registrationKeyId": "runners", "name": "runner-0",
                "daemonId": "daemon_1", "connected": true, "identityMode": "persistent",
            })),
            Some(
                json!({ "type": "registered", "registrationKeyId": "runners", "name": "runner-0" })
            )
        );
        // A daemon that registered itself is state, not configuration.
        assert_eq!(
            Environments::source_entry(&json!({
                "type": "registered", "registrationKeyId": "runners", "daemonId": "daemon_1",
            })),
            None
        );
        assert_eq!(
            Environments::source_entry(&json!({
                "type": "provisioned", "providerId": "pods", "templateId": "default",
                "targetId": "t-1", "observed": { "phase": "running" },
            })),
            Some(json!({ "type": "provisioned", "providerId": "pods", "templateId": "default" }))
        );
    }
}
