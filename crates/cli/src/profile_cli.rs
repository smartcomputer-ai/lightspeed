use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use api::{
    AgentApiErrorKind, AgentProfile, AgentProfileInput, EnvironmentLifecycleStatusView,
    EnvironmentProviderBindingListParams, EnvironmentProviderBindingStatusView,
    EnvironmentSourceView, InlineAgentProfile, ProfileApplyParams, ProfileDeleteParams, ProfileId,
    ProfileListParams, ProfilePutParams, ProfileReadParams, ProfileSource, WorkspaceLink,
    WorkspaceLinkAccess, WorkspaceLinkTarget,
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
    workspace_links: Option<bool>,
    mcp: Option<bool>,
    environments: Option<bool>,
}

impl ProvisionValidate {
    fn workspace_links(&self) -> bool {
        self.workspace_links.unwrap_or(true)
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
    link_path: String,
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
    let mut link_paths = BTreeSet::new();
    for entry in document.provision.vfs.clone() {
        if !link_paths.insert(entry.link_path.clone()) {
            bail!(
                "duplicate provision.vfs linkPath {}; each workspace link can be provisioned once",
                entry.link_path
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
                upsert_profile_link(
                    &mut document.profile,
                    &entry.link_path,
                    WorkspaceLinkTarget::Workspace { workspace_id },
                    WorkspaceLinkAccess::ReadWrite,
                )?;
            }
            ProvisionVfsMode::Snapshot => {
                upsert_profile_link(
                    &mut document.profile,
                    &entry.link_path,
                    WorkspaceLinkTarget::Snapshot {
                        snapshot_ref: summary.snapshot_ref,
                    },
                    WorkspaceLinkAccess::ReadOnly,
                )?;
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
    if document.provision.validate.workspace_links() {
        validate_workspace_links(api, document, provision_has_run, &mut report).await;
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
    let mut link_paths = BTreeSet::new();
    for entry in &provision.vfs {
        if !link_paths.insert(entry.link_path.clone()) {
            report.error(format!(
                "duplicate provision.vfs linkPath {}; each workspace link can be provisioned once",
                entry.link_path
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

async fn validate_workspace_links(
    api: &HttpAgentApi,
    document: &ProfileImportDocument,
    provision_has_run: bool,
    report: &mut ValidationReport,
) {
    let local_links = document
        .provision
        .vfs
        .iter()
        .map(|entry| entry.link_path.as_str())
        .collect::<BTreeSet<_>>();
    let Some(links) = profile_workspace_links(&document.profile) else {
        return;
    };
    for link in links {
        if !provision_has_run && local_links.contains(link.path.as_str()) {
            continue;
        }
        match &link.target {
            WorkspaceLinkTarget::Snapshot { snapshot_ref } => {
                if let Err(error) = api
                    .read_vfs_snapshot(api::VfsSnapshotReadParams {
                        snapshot_ref: snapshot_ref.clone(),
                    })
                    .await
                {
                    report.error(format!(
                        "workspace link {} references missing snapshot {}: {}",
                        link.path,
                        snapshot_ref,
                        api_error(error)
                    ));
                }
            }
            WorkspaceLinkTarget::Workspace { workspace_id } => {
                if let Err(error) = api
                    .read_vfs_workspace(api::VfsWorkspaceReadParams {
                        workspace_id: workspace_id.clone(),
                    })
                    .await
                {
                    report.error(format!(
                        "workspace link {} references missing workspace {}: {}",
                        link.path,
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
    let links = profile
        .document
        .config
        .as_ref()
        .and_then(|config| config.features.as_ref())
        .and_then(|features| features.mcp.as_ref())
        .map(|mcp| mcp.servers.as_slice())
        .unwrap_or_default();
    for link in links {
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
    }
}

async fn validate_environments(
    api: &HttpAgentApi,
    profile: &AgentProfileInput,
    report: &mut ValidationReport,
) {
    let Some(environment_id) = profile.document.active_environment_id.as_ref() else {
        return;
    };
    let environment = match api
        .read_environment(api::EnvironmentReadParams {
            environment_id: environment_id.clone(),
        })
        .await
    {
        Ok(response) => response.result.environment,
        Err(error) => {
            report.error(format!(
                "profile references missing environment {}: {}",
                environment_id,
                api_error(error)
            ));
            return;
        }
    };
    let bindings = match api
        .list_environment_provider_bindings(EnvironmentProviderBindingListParams::default())
        .await
    {
        Ok(response) => response.result.bindings,
        Err(error) => {
            report.error(format!(
                "failed to list environment provider bindings: {}",
                api_error(error)
            ));
            return;
        }
    };
    let bindings = bindings
        .into_iter()
        .map(|binding| (binding.binding_id.clone(), binding))
        .collect::<BTreeMap<_, _>>();
    if environment.status != EnvironmentLifecycleStatusView::Ready {
        report.warning(format!(
            "profile environment is {:?}, not ready",
            environment.status
        ));
    }
    if let EnvironmentSourceView::Provisioned { binding_id, .. } = &environment.source {
        match bindings.get(binding_id) {
            Some(binding) if binding.status != EnvironmentProviderBindingStatusView::Enabled => {
                report.warning(format!(
                    "profile environment binding {binding_id} is {:?}",
                    binding.status
                ));
            }
            None => report.warning(format!(
                "profile environment binding {binding_id} is missing"
            )),
            _ => {}
        }
    }
}

fn upsert_profile_link(
    profile: &mut AgentProfileInput,
    link_path: &str,
    target: WorkspaceLinkTarget,
    default_access: WorkspaceLinkAccess,
) -> Result<()> {
    let links = profile
        .document
        .config
        .as_mut()
        .and_then(|config| config.features.as_mut())
        .and_then(|features| features.vfs.as_mut())
        .map(|vfs| &mut vfs.workspace_links)
        .ok_or_else(|| anyhow!("profile provisioning requires config.features.vfs"))?;
    let target_is_snapshot = matches!(target, WorkspaceLinkTarget::Snapshot { .. });
    if let Some(link) = links.iter_mut().find(|link| link.path == link_path) {
        link.target = target;
        if target_is_snapshot && link.access == WorkspaceLinkAccess::ReadWrite {
            link.access = WorkspaceLinkAccess::ReadOnly;
        }
        return Ok(());
    }
    links.push(WorkspaceLink {
        path: link_path.to_owned(),
        target,
        access: default_access,
    });
    Ok(())
}

fn profile_workspace_links(profile: &AgentProfileInput) -> Option<&[WorkspaceLink]> {
    profile
        .document
        .config
        .as_ref()?
        .features
        .as_ref()?
        .vfs
        .as_ref()
        .map(|vfs| vfs.workspace_links.as_slice())
}

fn provision_workspace_id(profile: &AgentProfileInput, entry: &ProvisionVfs) -> String {
    entry
        .workspace_id
        .clone()
        .or_else(|| {
            profile_workspace_links(profile)
                .unwrap_or_default()
                .iter()
                .find(|link| link.path == entry.link_path)
                .and_then(|link| match &link.target {
                    WorkspaceLinkTarget::Workspace { workspace_id } => Some(workspace_id.clone()),
                    WorkspaceLinkTarget::Snapshot { .. } => None,
                })
        })
        .unwrap_or_else(|| {
            let link = sanitize_id_component(&entry.link_path);
            format!("profile_{}_{}", profile.profile_id.as_str(), link)
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
        "link".to_owned()
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
              "provision": {
                "vfs": [
                  {
                    "path": "./files",
                    "linkPath": "/workspace",
                    "mode": "workspace",
                    "workspaceId": "profile_support_workspace"
                  }
                ]
              }
            }"#,
        );

        assert_eq!(document.profile.profile_id.as_str(), "support");
        assert!(document.profile.document.config.is_none());
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
                      "linkPath": "/workspace",
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
        assert_eq!(batch.documents[1].provision.vfs[0].link_path, "/workspace");
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
    fn provisioned_workspace_link_is_inserted_when_missing() {
        let mut profile = AgentProfileInput {
            profile_id: ProfileId::new("support"),
            display_name: None,
            description: None,
            document: profile_document_with_vfs(Vec::new()),
        };
        upsert_profile_link(
            &mut profile,
            "/workspace",
            WorkspaceLinkTarget::Workspace {
                workspace_id: "profile_support_workspace".to_owned(),
            },
            WorkspaceLinkAccess::ReadWrite,
        )
        .unwrap();

        let links = profile_workspace_links(&profile).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].path, "/workspace");
        assert_eq!(links[0].access, WorkspaceLinkAccess::ReadWrite);
    }

    #[test]
    fn snapshot_workspace_link_forces_read_only_access() {
        let mut profile = AgentProfileInput {
            profile_id: ProfileId::new("support"),
            display_name: None,
            description: None,
            document: profile_document_with_vfs(vec![WorkspaceLink {
                path: "/workspace".to_owned(),
                target: WorkspaceLinkTarget::Workspace {
                    workspace_id: "profile_support_workspace".to_owned(),
                },
                access: WorkspaceLinkAccess::ReadWrite,
            }]),
        };
        upsert_profile_link(
            &mut profile,
            "/workspace",
            WorkspaceLinkTarget::Snapshot {
                snapshot_ref: format!("sha256:{}", "a".repeat(64)),
            },
            WorkspaceLinkAccess::ReadOnly,
        )
        .unwrap();

        let links = profile_workspace_links(&profile).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].access, WorkspaceLinkAccess::ReadOnly);
        assert!(matches!(
            links[0].target,
            WorkspaceLinkTarget::Snapshot { .. }
        ));
    }

    fn profile_document_with_vfs(workspace_links: Vec<WorkspaceLink>) -> api::ProfileDocument {
        api::ProfileDocument {
            config: Some(api::SessionConfig {
                model: None,
                generation: None,
                limits: None,
                context: None,
                features: Some(api::FeaturesConfig {
                    vfs: Some(api::VfsFeature {
                        version: api::CURRENT_FEATURE_VERSION,
                        workspace_links,
                        tools: None,
                        prompts: None,
                        skills: None,
                    }),
                    ..api::FeaturesConfig::default()
                }),
            }),
            ..api::ProfileDocument::default()
        }
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
