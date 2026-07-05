use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use api::{
    AgentApiErrorKind, AgentProfile, AgentProfileInput, EnvironmentProviderListParams,
    EnvironmentProviderStatusView, EnvironmentProviderTargetListParams,
    EnvironmentTargetStatusView, InlineAgentProfile, ProfileApplyParams, ProfileDeleteParams,
    ProfileId, ProfileListParams, ProfileMount, ProfilePutParams, ProfileReadParams,
    ProfileSource, VfsMountAccess, VfsMountSourceInput,
};
use clap::{Args, Subcommand};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::api_client::{HttpAgentApi, api_error};
use crate::vfs_transfer::{SnapshotUploadOptions, upload_snapshot_directory};

#[derive(Args, Debug)]
pub(crate) struct ProfilesArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[command(subcommand)]
    command: ProfilesCommand,
}

#[derive(Subcommand, Debug)]
enum ProfilesCommand {
    /// List profiles.
    List,
    /// Read one profile.
    Read { profile_id: String },
    /// Import a profile file, provisioning local resources and upserting the registry record.
    Import {
        json: String,
        /// Skip live resource validation after local provisioning.
        #[arg(long = "no-check")]
        no_check: bool,
    },
    /// Delete a profile.
    Delete { profile_id: String },
    /// Apply a profile to an idle session.
    Apply {
        session_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long = "profile-json")]
        profile_json: Option<String>,
        #[arg(long = "expected-config-revision")]
        expected_config_revision: Option<u64>,
        #[arg(long = "expected-tools-revision")]
        expected_tools_revision: Option<u64>,
    },
    /// Export a stored profile as an AgentProfileInput-shaped JSON document.
    Export {
        profile_id: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Validate a profile import document without modifying remote state.
    Check { json: String },
}

#[derive(Clone, Debug)]
struct ProfileImportDocument {
    profile: AgentProfileInput,
    provision: ProvisionConfig,
    base_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct ProfileImportBatch {
    documents: Vec<ProfileImportDocument>,
    source_was_array: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionConfig {
    #[serde(default)]
    vfs: Vec<ProvisionVfs>,
    #[serde(default)]
    validate: ProvisionValidate,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionValidate {
    mounts: Option<bool>,
    mcp: Option<bool>,
    environments: Option<bool>,
}

impl ProvisionValidate {
    fn mounts(&self) -> bool {
        self.mounts.unwrap_or(true)
    }

    fn mcp(&self) -> bool {
        self.mcp.unwrap_or(true)
    }

    fn environments(&self) -> bool {
        self.environments.unwrap_or(true)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionVfs {
    path: PathBuf,
    mount_path: String,
    #[serde(default)]
    mode: ProvisionVfsMode,
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ProvisionVfsMode {
    #[default]
    Workspace,
    Snapshot,
}

#[derive(Clone, Debug, Default)]
struct ValidationReport {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl ValidationReport {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }

    fn warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    fn extend(&mut self, other: ValidationReport) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }

    fn finish(self) -> Result<()> {
        for warning in &self.warnings {
            eprintln!("warning: {warning}");
        }
        if self.errors.is_empty() {
            println!("ok");
            return Ok(());
        }
        for error in &self.errors {
            eprintln!("error: {error}");
        }
        bail!("validation failed with {} error(s)", self.errors.len())
    }

    fn ensure_success(self) -> Result<()> {
        for warning in &self.warnings {
            eprintln!("warning: {warning}");
        }
        if self.errors.is_empty() {
            return Ok(());
        }
        for error in &self.errors {
            eprintln!("error: {error}");
        }
        bail!("validation failed with {} error(s)", self.errors.len())
    }
}

pub(crate) async fn handle(args: ProfilesArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    match args.command {
        ProfilesCommand::List => {
            let response = api
                .list_profiles(ProfileListParams::default())
                .await
                .map_err(api_error)?;
            print_json(&response.result.profiles)
        }
        ProfilesCommand::Read { profile_id } => {
            let response = api
                .read_profile(ProfileReadParams {
                    profile_id: parse_profile_id(&profile_id)?,
                })
                .await
                .map_err(api_error)?;
            print_json(&response.result.profile)
        }
        ProfilesCommand::Import { json, no_check } => {
            let mut batch = read_profile_import_arg(&json)?;
            for document in &mut batch.documents {
                let profile_id = document.profile.profile_id.as_str().to_owned();
                provision_vfs(&api, document)
                    .await
                    .with_context(|| format!("failed to provision profile {profile_id}"))?;
            }
            if !no_check {
                let report =
                    validate_import_documents(&api, &batch.documents, true, batch.source_was_array)
                        .await;
                report.ensure_success()?;
            }
            let mut profiles = Vec::with_capacity(batch.documents.len());
            for document in batch.documents {
                let profile_id = document.profile.profile_id.as_str().to_owned();
                let profile = upsert_profile(&api, document.profile)
                    .await
                    .with_context(|| format!("failed to import profile {profile_id}"))?;
                profiles.push(profile);
            }
            print_profile_import_results(profiles, batch.source_was_array)
        }
        ProfilesCommand::Delete { profile_id } => {
            let response = api
                .delete_profile(ProfileDeleteParams {
                    profile_id: parse_profile_id(&profile_id)?,
                })
                .await
                .map_err(api_error)?;
            print_json(&response.result.profile)
        }
        ProfilesCommand::Apply {
            session_id,
            profile,
            profile_json,
            expected_config_revision,
            expected_tools_revision,
        } => {
            let profile = profile_source_from_args(profile.as_deref(), profile_json.as_deref())?;
            let response = api
                .apply_profile(ProfileApplyParams {
                    session_id,
                    profile,
                    expected_config_revision,
                    expected_tools_revision,
                })
                .await
                .map_err(api_error)?;
            print_json(&response.result)
        }
        ProfilesCommand::Export { profile_id, out } => {
            let response = api
                .read_profile(ProfileReadParams {
                    profile_id: parse_profile_id(&profile_id)?,
                })
                .await
                .map_err(api_error)?;
            let profile = profile_input_from_record(response.result.profile);
            write_json_output(&profile, out.as_deref())
        }
        ProfilesCommand::Check { json } => {
            let batch = read_profile_import_arg(&json)?;
            validate_import_documents(&api, &batch.documents, false, batch.source_was_array)
                .await
                .finish()
        }
    }
}

async fn provision_vfs(api: &HttpAgentApi, document: &mut ProfileImportDocument) -> Result<()> {
    validate_local_vfs(&document.provision, &document.base_dir).ensure_success()?;
    let mut mount_paths = BTreeSet::new();
    for entry in document.provision.vfs.clone() {
        if !mount_paths.insert(entry.mount_path.clone()) {
            bail!(
                "duplicate provision.vfs mountPath {}; each mount can be provisioned once",
                entry.mount_path
            );
        }
        let source_path = resolve_local_path(&document.base_dir, &entry.path);
        let summary =
            upload_snapshot_directory(api, &source_path, SnapshotUploadOptions::default()).await?;
        match entry.mode {
            ProvisionVfsMode::Workspace => {
                let workspace_id = provision_workspace_id(&document.profile, &entry);
                upsert_vfs_workspace(api, workspace_id.clone(), summary.snapshot_ref.clone())
                    .await?;
                upsert_profile_mount(
                    &mut document.profile,
                    &entry.mount_path,
                    VfsMountSourceInput::Workspace { workspace_id },
                    VfsMountAccess::ReadWrite,
                );
            }
            ProvisionVfsMode::Snapshot => {
                upsert_profile_mount(
                    &mut document.profile,
                    &entry.mount_path,
                    VfsMountSourceInput::Snapshot {
                        snapshot_ref: summary.snapshot_ref,
                    },
                    VfsMountAccess::ReadOnly,
                );
            }
        }
    }
    Ok(())
}

async fn upsert_vfs_workspace(
    api: &HttpAgentApi,
    workspace_id: String,
    snapshot_ref: String,
) -> Result<api::VfsWorkspaceView> {
    match api
        .read_vfs_workspace(api::VfsWorkspaceReadParams {
            workspace_id: workspace_id.clone(),
        })
        .await
    {
        Ok(response) => {
            let workspace = response.result.workspace;
            if workspace.head_snapshot_ref == snapshot_ref {
                return Ok(workspace);
            }
            Ok(api
                .update_vfs_workspace(api::VfsWorkspaceUpdateParams {
                    workspace_id,
                    expected_revision: Some(workspace.revision),
                    snapshot_ref,
                    display_name: None,
                })
                .await
                .map_err(api_error)?
                .result
                .workspace)
        }
        Err(error) if matches!(error.kind, AgentApiErrorKind::NotFound) => Ok(api
            .create_vfs_workspace(api::VfsWorkspaceCreateParams {
                workspace_id: Some(workspace_id),
                snapshot_ref: Some(snapshot_ref),
                display_name: None,
            })
            .await
            .map_err(api_error)?
            .result
            .workspace),
        Err(error) => Err(api_error(error)),
    }
}

async fn upsert_profile(api: &HttpAgentApi, profile: AgentProfileInput) -> Result<AgentProfile> {
    match api
        .read_profile(ProfileReadParams {
            profile_id: profile.profile_id.clone(),
        })
        .await
    {
        Ok(response) => {
            let current = response.result.profile;
            if profile_record_matches_input(&current, &profile) {
                return Ok(current);
            }
            Ok(api
                .put_profile(ProfilePutParams {
                    profile,
                    expected_revision: Some(current.revision),
                })
                .await
                .map_err(api_error)?
                .result
                .profile)
        }
        Err(error) if matches!(error.kind, AgentApiErrorKind::NotFound) => Ok(api
            .put_profile(ProfilePutParams {
                profile,
                expected_revision: None,
            })
            .await
            .map_err(api_error)?
            .result
            .profile),
        Err(error) => Err(api_error(error)),
    }
}

async fn validate_import_document(
    api: &HttpAgentApi,
    document: &ProfileImportDocument,
    provision_has_run: bool,
) -> ValidationReport {
    let mut report = ValidationReport::default();
    report.extend(validate_local_vfs(&document.provision, &document.base_dir));
    if document.provision.validate.mounts() {
        validate_mounts(api, document, provision_has_run, &mut report).await;
    }
    if document.provision.validate.mcp() {
        validate_mcp(api, &document.profile, &mut report).await;
    }
    if document.provision.validate.environments() {
        validate_environments(api, &document.profile, &mut report).await;
    }
    report
}

async fn validate_import_documents(
    api: &HttpAgentApi,
    documents: &[ProfileImportDocument],
    provision_has_run: bool,
    prefix_messages: bool,
) -> ValidationReport {
    let mut report = ValidationReport::default();
    for document in documents {
        let document_report = validate_import_document(api, document, provision_has_run).await;
        if prefix_messages {
            report.extend(prefix_validation_report(
                document.profile.profile_id.as_str(),
                document_report,
            ));
        } else {
            report.extend(document_report);
        }
    }
    report
}

fn prefix_validation_report(profile_id: &str, mut report: ValidationReport) -> ValidationReport {
    report.warnings = report
        .warnings
        .into_iter()
        .map(|message| format!("profile {profile_id}: {message}"))
        .collect();
    report.errors = report
        .errors
        .into_iter()
        .map(|message| format!("profile {profile_id}: {message}"))
        .collect();
    report
}

fn validate_local_vfs(provision: &ProvisionConfig, base_dir: &Path) -> ValidationReport {
    let mut report = ValidationReport::default();
    let mut mount_paths = BTreeSet::new();
    for entry in &provision.vfs {
        if !mount_paths.insert(entry.mount_path.clone()) {
            report.error(format!(
                "duplicate provision.vfs mountPath {}; each mount can be provisioned once",
                entry.mount_path
            ));
        }
        let path = resolve_local_path(base_dir, &entry.path);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                report.error(format!(
                    "provision.vfs path must not be a symlink: {}",
                    path.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                report.error(format!(
                    "provision.vfs path must be a directory: {}",
                    path.display()
                ));
            }
            Ok(_) => {
                if let Err(error) = fs::read_dir(&path) {
                    report.error(format!(
                        "provision.vfs path is not readable: {}: {error}",
                        path.display()
                    ));
                }
            }
            Err(error) => {
                report.error(format!(
                    "provision.vfs path does not exist or is not inspectable: {}: {error}",
                    path.display()
                ));
            }
        }
    }
    report
}

async fn validate_mounts(
    api: &HttpAgentApi,
    document: &ProfileImportDocument,
    provision_has_run: bool,
    report: &mut ValidationReport,
) {
    let local_mounts = document
        .provision
        .vfs
        .iter()
        .map(|entry| entry.mount_path.as_str())
        .collect::<BTreeSet<_>>();
    for mount in &document.profile.document.mounts {
        if !provision_has_run && local_mounts.contains(mount.mount_path.as_str()) {
            continue;
        }
        match &mount.source {
            VfsMountSourceInput::Snapshot { snapshot_ref } => {
                if let Err(error) = api
                    .read_vfs_snapshot(api::VfsSnapshotReadParams {
                        snapshot_ref: snapshot_ref.clone(),
                    })
                    .await
                {
                    report.error(format!(
                        "mount {} references missing snapshot {}: {}",
                        mount.mount_path,
                        snapshot_ref,
                        api_error(error)
                    ));
                }
            }
            VfsMountSourceInput::Workspace { workspace_id } => {
                if let Err(error) = api
                    .read_vfs_workspace(api::VfsWorkspaceReadParams {
                        workspace_id: workspace_id.clone(),
                    })
                    .await
                {
                    report.error(format!(
                        "mount {} references missing workspace {}: {}",
                        mount.mount_path,
                        workspace_id,
                        api_error(error)
                    ));
                }
            }
        }
    }
}

async fn validate_mcp(
    api: &HttpAgentApi,
    profile: &AgentProfileInput,
    report: &mut ValidationReport,
) {
    for link in &profile.document.mcp {
        if let Err(error) = api
            .read_mcp_server(api::McpServerReadParams {
                server_id: link.server_id.clone(),
            })
            .await
        {
            report.error(format!(
                "mcp server {} is not registered: {}",
                link.server_id,
                api_error(error)
            ));
        }
        if let Some(grant_id) = &link.auth_grant_id
            && let Err(error) = api
                .read_auth_grant(api::AuthGrantReadParams {
                    grant_id: grant_id.clone(),
                })
                .await
        {
            report.error(format!(
                "mcp server {} references missing auth grant {}: {}",
                link.server_id,
                grant_id,
                api_error(error)
            ));
        }
    }
}

async fn validate_environments(
    api: &HttpAgentApi,
    profile: &AgentProfileInput,
    report: &mut ValidationReport,
) {
    if profile.document.environments.is_empty() {
        return;
    }
    let providers = match api
        .list_environment_providers(EnvironmentProviderListParams::default())
        .await
    {
        Ok(response) => response.result.providers,
        Err(error) => {
            report.error(format!(
                "failed to list environment providers: {}",
                api_error(error)
            ));
            return;
        }
    };
    let providers = providers
        .into_iter()
        .map(|provider| (provider.provider_id.clone(), provider))
        .collect::<BTreeMap<_, _>>();
    let mut target_cache = BTreeMap::new();
    for environment in &profile.document.environments {
        let Some(provider) = providers.get(&environment.provider_id) else {
            report.error(format!(
                "environment {} references missing provider {}",
                environment.env_id, environment.provider_id
            ));
            continue;
        };
        if !provider.capabilities.attach_target {
            report.error(format!(
                "environment {} references provider {} which does not support target attachment",
                environment.env_id, environment.provider_id
            ));
            continue;
        }
        if !provider.capabilities.list_targets {
            report.warning(format!(
                "environment {} target {} was not validated because provider {} does not list targets",
                environment.env_id, environment.target_id, environment.provider_id
            ));
            continue;
        }
        if !target_cache.contains_key(&environment.provider_id) {
            let targets = match api
                .list_environment_provider_targets(EnvironmentProviderTargetListParams {
                    provider_id: environment.provider_id.clone(),
                    status: None,
                })
                .await
            {
                Ok(response) => response.result.targets,
                Err(error) => {
                    report.error(format!(
                        "failed to list targets for provider {}: {}",
                        environment.provider_id,
                        api_error(error)
                    ));
                    continue;
                }
            };
            target_cache.insert(environment.provider_id.clone(), targets);
        }
        let targets = target_cache
            .get(&environment.provider_id)
            .expect("target cache entry inserted");
        let Some(target) = targets
            .iter()
            .find(|target| target.target_id == environment.target_id)
        else {
            report.error(format!(
                "environment {} references missing target {} on provider {}",
                environment.env_id, environment.target_id, environment.provider_id
            ));
            continue;
        };
        if target.status != EnvironmentTargetStatusView::Ready {
            report.warning(format!(
                "environment {} target {} on provider {} is {:?}, not ready",
                environment.env_id, environment.target_id, environment.provider_id, target.status
            ));
        }
        if provider.status != EnvironmentProviderStatusView::Online {
            report.warning(format!(
                "environment {} provider {} is {:?}, not online",
                environment.env_id, environment.provider_id, provider.status
            ));
        }
    }
}

fn upsert_profile_mount(
    profile: &mut AgentProfileInput,
    mount_path: &str,
    source: VfsMountSourceInput,
    default_access: VfsMountAccess,
) {
    let source_is_snapshot = matches!(source, VfsMountSourceInput::Snapshot { .. });
    if let Some(mount) = profile
        .document
        .mounts
        .iter_mut()
        .find(|mount| mount.mount_path == mount_path)
    {
        mount.source = source;
        if source_is_snapshot && mount.access == VfsMountAccess::ReadWrite {
            mount.access = VfsMountAccess::ReadOnly;
        }
        return;
    }
    profile.document.mounts.push(ProfileMount {
        mount_path: mount_path.to_owned(),
        source,
        access: default_access,
    });
}

fn provision_workspace_id(profile: &AgentProfileInput, entry: &ProvisionVfs) -> String {
    entry
        .workspace_id
        .clone()
        .or_else(|| {
            profile
                .document
                .mounts
                .iter()
                .find(|mount| mount.mount_path == entry.mount_path)
                .and_then(|mount| match &mount.source {
                    VfsMountSourceInput::Workspace { workspace_id } => Some(workspace_id.clone()),
                    VfsMountSourceInput::Snapshot { .. } => None,
                })
        })
        .unwrap_or_else(|| {
            let mount = sanitize_id_component(&entry.mount_path);
            format!("profile_{}_{}", profile.profile_id.as_str(), mount)
        })
}

fn sanitize_id_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "mount".to_owned()
    } else {
        out.to_owned()
    }
}

fn profile_record_matches_input(record: &AgentProfile, input: &AgentProfileInput) -> bool {
    record.profile_id == input.profile_id
        && record.display_name == input.display_name
        && record.description == input.description
        && record.document == input.document
}

fn profile_input_from_record(record: AgentProfile) -> AgentProfileInput {
    AgentProfileInput {
        profile_id: record.profile_id,
        display_name: record.display_name,
        description: record.description,
        document: record.document,
    }
}

fn parse_profile_id(value: &str) -> Result<ProfileId> {
    ProfileId::try_new(value.to_owned()).map_err(|error| anyhow!("invalid profile id: {error}"))
}

fn profile_source_from_args(
    profile: Option<&str>,
    profile_json: Option<&str>,
) -> Result<ProfileSource> {
    match (profile, profile_json) {
        (Some(_), Some(_)) => Err(anyhow!(
            "--profile and --profile-json are mutually exclusive"
        )),
        (Some(profile_id), None) => Ok(ProfileSource::Named {
            profile_id: parse_profile_id(profile_id)?,
        }),
        (None, Some(json_arg)) => {
            let profile = read_json_arg::<InlineAgentProfile>(json_arg)?;
            Ok(ProfileSource::Inline { profile })
        }
        (None, None) => Err(anyhow!("one of --profile or --profile-json is required")),
    }
}

fn read_profile_import_arg(arg: &str) -> Result<ProfileImportBatch> {
    let input = read_json_text_arg(arg)?;
    read_profile_import_json(&input.json, input.base_dir)
}

fn read_profile_import_json(json: &str, base_dir: PathBuf) -> Result<ProfileImportBatch> {
    let value = serde_json::from_str::<Value>(json).context("failed to parse JSON")?;
    match value {
        Value::Array(values) => {
            if values.is_empty() {
                bail!("profile import document array must contain at least one profile");
            }
            let mut documents = Vec::with_capacity(values.len());
            for (index, value) in values.into_iter().enumerate() {
                let document = profile_import_document_from_value(value, base_dir.clone())
                    .with_context(|| {
                        format!("failed to parse profile import document at array index {index}")
                    })?;
                documents.push(document);
            }
            ensure_unique_profile_ids(&documents)?;
            Ok(ProfileImportBatch {
                documents,
                source_was_array: true,
            })
        }
        Value::Object(_) => Ok(ProfileImportBatch {
            documents: vec![profile_import_document_from_value(value, base_dir)?],
            source_was_array: false,
        }),
        _ => bail!("profile import document must be a JSON object or array of objects"),
    }
}

fn profile_import_document_from_value(
    mut value: Value,
    base_dir: PathBuf,
) -> Result<ProfileImportDocument> {
    let provision = match value.as_object_mut() {
        Some(object) => object.remove("provision"),
        None => bail!("profile import document must be a JSON object"),
    };
    let provision = provision
        .map(serde_json::from_value::<Option<ProvisionConfig>>)
        .transpose()
        .context("failed to parse provision")?
        .flatten()
        .unwrap_or_default();
    let profile = serde_json::from_value::<AgentProfileInput>(value)
        .context("failed to parse AgentProfileInput")?;
    Ok(ProfileImportDocument {
        profile,
        provision,
        base_dir,
    })
}

fn ensure_unique_profile_ids(documents: &[ProfileImportDocument]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for document in documents {
        let profile_id = document.profile.profile_id.as_str();
        if !ids.insert(profile_id.to_owned()) {
            bail!("duplicate profileId {profile_id} in profile import array");
        }
    }
    Ok(())
}

fn read_json_arg<T: DeserializeOwned>(arg: &str) -> Result<T> {
    let input = read_json_text_arg(arg)?;
    serde_json::from_str(&input.json).context("failed to parse JSON")
}

fn print_profile_import_results(profiles: Vec<AgentProfile>, source_was_array: bool) -> Result<()> {
    if source_was_array {
        print_json(&profiles)
    } else {
        let profile = profiles
            .into_iter()
            .next()
            .context("single-profile import produced no profile")?;
        print_json(&profile)
    }
}

struct JsonTextArg {
    json: String,
    base_dir: PathBuf,
}

fn read_json_text_arg(arg: &str) -> Result<JsonTextArg> {
    if arg == "-" {
        let mut json = String::new();
        io::stdin()
            .read_to_string(&mut json)
            .context("failed to read JSON from stdin")?;
        return Ok(JsonTextArg {
            json,
            base_dir: std::env::current_dir().context("failed to resolve current directory")?,
        });
    }
    let path = PathBuf::from(arg);
    if path.exists() {
        let json = fs::read_to_string(&path)
            .with_context(|| format!("failed to read JSON {}", path.display()))?;
        let base_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok(JsonTextArg { json, base_dir });
    }
    Ok(JsonTextArg {
        json: arg.to_owned(),
        base_dir: std::env::current_dir().context("failed to resolve current directory")?,
    })
}

fn resolve_local_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn write_json_output(value: &impl serde::Serialize, out: Option<&Path>) -> Result<()> {
    let json = format!("{}\n", serde_json::to_string_pretty(value)?);
    match out {
        Some(path) => fs::write(path, json)
            .with_context(|| format!("failed to write JSON {}", path.display())),
        None => {
            print!("{json}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_document_strips_provision() {
        let document = parse_import_document_for_test(
            r#"{
              "profileId": "support",
              "mounts": [],
              "provision": {
                "vfs": [
                  {
                    "path": "./files",
                    "mountPath": "/workspace",
                    "mode": "workspace",
                    "workspaceId": "profile_support_workspace"
                  }
                ]
              }
            }"#,
        );

        assert_eq!(document.profile.profile_id.as_str(), "support");
        assert_eq!(document.profile.document.mounts.len(), 0);
        assert_eq!(document.provision.vfs.len(), 1);
        assert_eq!(
            document.provision.vfs[0].workspace_id.as_deref(),
            Some("profile_support_workspace")
        );
    }

    #[test]
    fn import_document_accepts_profile_array() {
        let batch = parse_import_batch_for_test(
            r#"[
              {
                "profileId": "support",
                "displayName": "Support"
              },
              {
                "profileId": "review",
                "provision": {
                  "vfs": [
                    {
                      "path": "./review-files",
                      "mountPath": "/workspace",
                      "mode": "snapshot"
                    }
                  ]
                }
              }
            ]"#,
        );

        assert!(batch.source_was_array);
        assert_eq!(batch.documents.len(), 2);
        assert_eq!(batch.documents[0].profile.profile_id.as_str(), "support");
        assert_eq!(batch.documents[1].profile.profile_id.as_str(), "review");
        assert_eq!(batch.documents[1].provision.vfs.len(), 1);
        assert_eq!(batch.documents[1].provision.vfs[0].mount_path, "/workspace");
    }

    #[test]
    fn import_document_rejects_duplicate_profile_ids_in_array() {
        let result = read_profile_import_json(
            r#"[
              { "profileId": "support" },
              { "profileId": "support" }
            ]"#,
            PathBuf::from("."),
        );

        assert!(result.is_err());
    }

    #[test]
    fn provisioned_mount_is_inserted_when_missing() {
        let mut profile = AgentProfileInput {
            profile_id: ProfileId::new("support"),
            display_name: None,
            description: None,
            document: api::ProfileDocument::default(),
        };
        upsert_profile_mount(
            &mut profile,
            "/workspace",
            VfsMountSourceInput::Workspace {
                workspace_id: "profile_support_workspace".to_owned(),
            },
            VfsMountAccess::ReadWrite,
        );

        assert_eq!(profile.document.mounts.len(), 1);
        assert_eq!(profile.document.mounts[0].mount_path, "/workspace");
        assert_eq!(profile.document.mounts[0].access, VfsMountAccess::ReadWrite);
    }

    #[test]
    fn snapshot_mount_forces_read_only_access() {
        let mut profile = AgentProfileInput {
            profile_id: ProfileId::new("support"),
            display_name: None,
            description: None,
            document: api::ProfileDocument {
                mounts: vec![ProfileMount {
                    mount_path: "/workspace".to_owned(),
                    source: VfsMountSourceInput::Workspace {
                        workspace_id: "profile_support_workspace".to_owned(),
                    },
                    access: VfsMountAccess::ReadWrite,
                }],
                ..api::ProfileDocument::default()
            },
        };
        upsert_profile_mount(
            &mut profile,
            "/workspace",
            VfsMountSourceInput::Snapshot {
                snapshot_ref: format!("sha256:{}", "a".repeat(64)),
            },
            VfsMountAccess::ReadOnly,
        );

        assert_eq!(profile.document.mounts.len(), 1);
        assert_eq!(profile.document.mounts[0].access, VfsMountAccess::ReadOnly);
        assert!(matches!(
            profile.document.mounts[0].source,
            VfsMountSourceInput::Snapshot { .. }
        ));
    }

    fn parse_import_document_for_test(json: &str) -> ProfileImportDocument {
        let mut batch = parse_import_batch_for_test(json);
        assert!(!batch.source_was_array);
        assert_eq!(batch.documents.len(), 1);
        batch.documents.pop().unwrap()
    }

    fn parse_import_batch_for_test(json: &str) -> ProfileImportBatch {
        read_profile_import_json(json, PathBuf::from(".")).unwrap()
    }
}
