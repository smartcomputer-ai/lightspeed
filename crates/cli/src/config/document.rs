//! Configuration documents: the files `config apply` and `config diff` read
//! and `config export` writes.
//!
//! A document is an envelope around the API's own inputs. The envelope says
//! which universe (or the deployment) the entries belong to and groups them
//! by resource; an entry is the payload of the method that writes the
//! resource, with a secret replaced by where it comes from.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The layout version this CLI reads and writes. It identifies the shape of
/// the file only; it takes no part in deciding what changed.
pub(crate) const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Kind {
    Universe,
    Deployment,
}

/// One universe's configuration. Every section is optional; an absent
/// section is not managed by the document.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UniverseDocument {
    pub schema_version: u32,
    pub kind: Kind,
    /// The universe's deployment-unique name.
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<api::DeclaredUniverseMember>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_group_mappings: Option<Vec<api::UniverseDirectoryGroupMapping>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_defaults: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_providers: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_keys: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_keys: Option<Vec<Value>>,
}

/// Resources that belong to the deployment and not to one universe.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeploymentDocument {
    pub schema_version: u32,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_keys: Option<Vec<Value>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Document {
    Universe(Box<UniverseDocument>),
    Deployment(DeploymentDocument),
}

/// A document and the file it came from. Relative paths inside it (includes
/// and secret files) are resolved against that file's directory.
#[derive(Clone, Debug)]
pub(crate) struct LoadedDocument {
    pub path: PathBuf,
    pub document: Document,
}

impl LoadedDocument {
    pub(crate) fn directory(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// What the document configures, for messages.
    pub(crate) fn subject(&self) -> String {
        match &self.document {
            Document::Universe(universe) => format!("universe {}", universe.handle),
            Document::Deployment(_) => "deployment".to_owned(),
        }
    }
}

/// Load every document under the given files and directories. A directory is
/// read recursively; a file in it is a document when it is YAML or JSON and
/// has a `kind`, so included entry files can live beside the documents.
pub(crate) fn load(paths: &[PathBuf]) -> Result<Vec<LoadedDocument>> {
    let mut documents = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut files = Vec::new();
            collect_files(path, &mut files)?;
            files.sort();
            for file in files {
                let value = read_value(&file)?;
                // Entry files included from a document have no envelope.
                if value.get("kind").is_some() {
                    documents.push(parse(&file, value)?);
                }
            }
        } else {
            documents.push(parse(path, read_value(path)?)?);
        }
    }
    if documents.is_empty() {
        bail!("no configuration document found");
    }
    // Two documents for one universe would fight over it.
    let mut seen = BTreeMap::new();
    for loaded in &documents {
        if let Some(previous) = seen.insert(loaded.subject(), &loaded.path) {
            bail!(
                "{} is configured by both {} and {}",
                loaded.subject(),
                previous.display(),
                loaded.path.display()
            );
        }
    }
    Ok(documents)
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("read directory {}", directory.display()))?;
    for entry in entries {
        let entry = entry?;
        // Hidden entries are not configuration. A mounted Kubernetes
        // ConfigMap or Secret keeps its files in hidden directories
        // (`..data`, `..<timestamp>`) and links to them from the top level,
        // so following those would read every document more than once.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if is_document_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_document_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("yaml" | "yml" | "json")
    )
}

/// Read a YAML or JSON file as plain JSON data.
pub(crate) fn read_value(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let value = if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
        serde_json::from_str(&text).map_err(anyhow::Error::from)
    } else {
        serde_yaml::from_str::<Value>(&text).map_err(anyhow::Error::from)
    };
    value.with_context(|| format!("parse {}", path.display()))
}

fn parse(path: &Path, mut value: Value) -> Result<LoadedDocument> {
    let located = || format!("in {}", path.display());
    let version = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .with_context(|| format!("schemaVersion is missing {}", located()))?;
    if version != u64::from(SCHEMA_VERSION) {
        bail!(
            "schemaVersion {version} {} is not supported; this CLI reads version {SCHEMA_VERSION}",
            located()
        );
    }
    resolve_includes(path, &mut value)?;
    let kind: Kind = serde_json::from_value(value.get("kind").cloned().unwrap_or(Value::Null))
        .with_context(|| format!("kind must be universe or deployment {}", located()))?;
    let document = match kind {
        Kind::Universe => {
            let document: UniverseDocument = serde_json::from_value(value).with_context(located)?;
            if document.handle.trim().is_empty() {
                bail!("handle is empty {}", located());
            }
            Document::Universe(Box::new(document))
        }
        Kind::Deployment => {
            Document::Deployment(serde_json::from_value(value).with_context(located)?)
        }
    };
    Ok(LoadedDocument {
        path: path.to_path_buf(),
        document,
    })
}

/// Replace every list entry of the form `{ file: PATH }` with the entry that
/// file holds. Paths are relative to the including file.
fn resolve_includes(path: &Path, document: &mut Value) -> Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let Some(sections) = document.as_object_mut() else {
        return Ok(());
    };
    for (section, entries) in sections.iter_mut() {
        let Some(entries) = entries.as_array_mut() else {
            continue;
        };
        for entry in entries.iter_mut() {
            let Some(include) = include_path(entry) else {
                continue;
            };
            let file = directory.join(include);
            let included = read_value(&file)
                .with_context(|| format!("{section} entry included from {}", path.display()))?;
            if !included.is_object() {
                bail!("{} must hold one {section} entry", file.display());
            }
            *entry = included;
        }
    }
    Ok(())
}

fn include_path(entry: &Value) -> Option<&str> {
    let object = entry.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object.get("file")?.as_str()
}

/// Where a secret comes from. A document never holds the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum SecretSource {
    /// An environment variable of the process running the command.
    Env(String),
    /// A file, relative to the document. One trailing line break is dropped.
    File(PathBuf),
}

impl SecretSource {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Env(name) => format!("environment variable {name}"),
            Self::File(path) => format!("file {}", path.display()),
        }
    }

    pub(crate) fn resolve(&self, directory: &Path) -> Result<String> {
        let value = match self {
            Self::Env(name) => std::env::var(name)
                .map_err(|_| anyhow::anyhow!("environment variable {name} is not set"))?,
            Self::File(path) => {
                let path = directory.join(path);
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("read secret file {}", path.display()))?;
                text.strip_suffix('\n')
                    .map(|text| text.strip_suffix('\r').unwrap_or(text))
                    .unwrap_or(&text)
                    .to_owned()
            }
        };
        if value.is_empty() {
            bail!("{} is empty", self.describe());
        }
        Ok(value)
    }
}

/// The document field that names the source of the wire field `secret`:
/// `token` is declared as `tokenFrom`.
pub(crate) fn source_field(secret: &str) -> String {
    format!("{secret}From")
}

/// A placeholder source for an exported secret, named after the resource:
/// `tracker-token` becomes the environment variable `TRACKER_TOKEN`.
pub(crate) fn placeholder_source(id: &str) -> SecretSource {
    let mut name: String = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if name
        .chars()
        .next()
        .is_none_or(|first| first.is_ascii_digit())
    {
        name.insert(0, '_');
    }
    SecretSource::Env(name)
}

/// The syntax `export` writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Format {
    Yaml,
    Json,
}

impl Format {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Yaml => "yaml",
            Self::Json => "json",
        }
    }

    pub(crate) fn render(self, value: &impl Serialize) -> Result<String> {
        Ok(match self {
            Self::Yaml => serde_yaml::to_string(value)?,
            Self::Json => format!("{}\n", serde_json::to_string_pretty(value)?),
        })
    }
}

impl UniverseDocument {
    pub(crate) fn configuration(&self) -> api::UniverseConfiguration {
        api::UniverseConfiguration {
            display_name: self.display_name.clone(),
            members: self.members.clone(),
            directory_group_mappings: self.directory_group_mappings.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(directory: &Path, name: &str, text: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn yaml_and_json_documents_load_alike_and_includes_are_resolved() {
        let directory = tempfile::tempdir().unwrap();
        write(
            directory.path(),
            "universes/research.yaml",
            "schemaVersion: 1\nkind: universe\nhandle: research\nprofiles:\n  - file: ../profiles/reviewer.json\n  - profileId: inline\n",
        );
        write(
            directory.path(),
            "profiles/reviewer.json",
            r#"{"profileId":"reviewer","displayName":"Reviewer"}"#,
        );
        write(
            directory.path(),
            "deployment.json",
            r#"{"schemaVersion":1,"kind":"deployment","apiKeys":[]}"#,
        );
        let documents = load(&[directory.path().to_path_buf()]).unwrap();
        assert_eq!(documents.len(), 2, "the included profile is no document");
        let universe = documents
            .iter()
            .find_map(|loaded| match &loaded.document {
                Document::Universe(universe) => Some(universe),
                Document::Deployment(_) => None,
            })
            .expect("universe document");
        assert_eq!(universe.handle, "research");
        let profiles = universe.profiles.as_ref().unwrap();
        assert_eq!(profiles[0]["profileId"], "reviewer");
        assert_eq!(profiles[0]["displayName"], "Reviewer");
        assert_eq!(profiles[1]["profileId"], "inline");
        assert!(
            universe.credentials.is_none(),
            "absent sections stay absent"
        );
    }

    /// A ConfigMap volume: the files live in a timestamped hidden directory,
    /// `..data` links to it, and each visible name links through `..data`.
    #[cfg(unix)]
    #[test]
    fn a_mounted_configmap_is_read_once_through_its_visible_names() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let mount = directory.path().join("config");
        let revision = mount.join("..2026_11_13_08_00_00.1234");
        std::fs::create_dir_all(&revision).unwrap();
        std::fs::write(
            revision.join("research.yaml"),
            "schemaVersion: 1\nkind: universe\nhandle: research\nprofiles:\n  - file: reviewer.json\n",
        )
        .unwrap();
        std::fs::write(
            revision.join("reviewer.json"),
            r#"{"profileId":"reviewer"}"#,
        )
        .unwrap();
        symlink("..2026_11_13_08_00_00.1234", mount.join("..data")).unwrap();
        symlink("..data/research.yaml", mount.join("research.yaml")).unwrap();
        symlink("..data/reviewer.json", mount.join("reviewer.json")).unwrap();

        let documents = load(std::slice::from_ref(&mount)).unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].path, mount.join("research.yaml"));
        let Document::Universe(universe) = &documents[0].document else {
            panic!("expected a universe document");
        };
        // An include next to the document resolves through the mount too.
        assert_eq!(
            universe.profiles.as_ref().unwrap()[0]["profileId"],
            "reviewer"
        );
    }

    #[test]
    fn a_document_with_an_unknown_field_version_or_duplicate_subject_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let misspelled = write(
            directory.path(),
            "a.yaml",
            "schemaVersion: 1\nkind: universe\nhandle: a\nprofile: []\n",
        );
        let error = format!("{:#}", load(&[misspelled]).unwrap_err());
        assert!(error.contains("unknown field `profile`"), "{error}");

        let future = write(
            directory.path(),
            "b.yaml",
            "schemaVersion: 2\nkind: universe\nhandle: b\n",
        );
        let error = format!("{:#}", load(&[future]).unwrap_err());
        assert!(error.contains("schemaVersion 2"), "{error}");

        let first = write(
            directory.path(),
            "c.yaml",
            "schemaVersion: 1\nkind: universe\nhandle: same\n",
        );
        let second = write(
            directory.path(),
            "d.yaml",
            "schemaVersion: 1\nkind: universe\nhandle: same\n",
        );
        let error = format!("{:#}", load(&[first, second]).unwrap_err());
        assert!(error.contains("configured by both"), "{error}");
    }

    #[test]
    fn secret_sources_resolve_from_files_and_refuse_missing_or_empty_values() {
        let directory = tempfile::tempdir().unwrap();
        write(directory.path(), "token", "s3cret\n");
        write(directory.path(), "empty", "\n");
        let source: SecretSource = serde_json::from_str(r#"{"file":"token"}"#).unwrap();
        assert_eq!(source.resolve(directory.path()).unwrap(), "s3cret");
        assert!(
            SecretSource::File("empty".into())
                .resolve(directory.path())
                .is_err()
        );
        assert!(
            SecretSource::Env("LIGHTSPEED_TEST_SECRET_THAT_IS_NOT_SET".to_owned())
                .resolve(directory.path())
                .is_err()
        );
        // A source names exactly one place.
        assert!(serde_json::from_str::<SecretSource>(r#"{"env":"A","file":"b"}"#).is_err());
        assert_eq!(
            placeholder_source("tracker-token"),
            SecretSource::Env("TRACKER_TOKEN".to_owned())
        );
        assert_eq!(source_field("token"), "tokenFrom");
    }
}
