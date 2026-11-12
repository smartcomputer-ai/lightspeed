//! Configuration as documents (`lightspeed config …`): `export` writes the
//! configuration of a universe as a file, `apply` makes a universe match a
//! file, `diff` reports what `apply` would change, and `schema` prints the
//! JSON Schema of such a file.
//!
//! All three are ordinary clients of the public API. A document holds the
//! API's own inputs, so there is nothing here the other commands could not
//! do one resource at a time.

mod document;
mod export;
mod plan;
mod resources;
mod schema;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use self::{
    document::{Document, Format, LoadedDocument},
    plan::{Action, DocumentPlan, Step},
};
use crate::{api_client::HttpAgentApi, connection::ResolvedConnection};

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum ConfigCommand {
    /// Write the configuration of the selected universe, or of the
    /// deployment, as a document.
    Export(ExportArgs),
    /// Report what `apply` would change, without writing anything. Exits
    /// with status 2 when something would change.
    Diff(DocumentArgs),
    /// Make universes match configuration documents. Safe to repeat: a
    /// document that already matches changes nothing.
    Apply(DocumentArgs),
    /// Print the JSON Schema of a configuration document, for editors and
    /// other tools to validate against. Contacts no runtime.
    Schema,
}

impl ConfigArgs {
    /// Whether the command runs without a runtime connection.
    pub(crate) fn offline(&self) -> bool {
        matches!(self.command, ConfigCommand::Schema)
    }
}

pub(crate) async fn handle(args: ConfigArgs) -> Result<()> {
    match args.command {
        ConfigCommand::Export(args) => export(args).await,
        ConfigCommand::Diff(args) => diff(args).await,
        ConfigCommand::Apply(args) => apply(args).await,
        ConfigCommand::Schema => {
            print!("{}", schema::rendered());
            Ok(())
        }
    }
}

#[derive(Args, Debug, Clone)]
pub(crate) struct DocumentArgs {
    /// A configuration document, or a directory of them (read recursively).
    /// YAML and JSON are both accepted.
    #[arg(short = 'f', long = "file", required = true, num_args = 1..)]
    files: Vec<PathBuf>,
    /// Print one JSON object per resource instead of lines for people.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ExportArgs {
    /// Export the deployment's own resources instead of a universe's.
    #[arg(long)]
    deployment: bool,
    /// Write the document to this file instead of standard output.
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,
    /// The syntax to write; taken from the output file's extension when
    /// omitted, and YAML otherwise.
    #[arg(long, value_enum)]
    format: Option<Format>,
    /// Write each profile to a JSON file in this directory and include it
    /// from the document.
    #[arg(long = "profiles-dir", requires = "output")]
    profiles_dir: Option<PathBuf>,
}

fn base_connection() -> ResolvedConnection {
    HttpAgentApi::new("").connection().clone()
}

/// The deployment's document is planned and written first: universes may
/// depend on what it declares.
fn in_order(mut documents: Vec<LoadedDocument>) -> Vec<LoadedDocument> {
    documents.sort_by_key(|loaded| match loaded.document {
        Document::Deployment(_) => 0,
        Document::Universe(_) => 1,
    });
    documents
}

async fn plan_all(files: &[PathBuf]) -> Result<Vec<DocumentPlan>> {
    let documents = in_order(document::load(files)?);
    let base = base_connection();
    let mut plans = Vec::new();
    for loaded in &documents {
        plans.push(
            plan::plan(&base, loaded)
                .await
                .with_context(|| format!("{}", loaded.path.display()))?,
        );
    }
    Ok(plans)
}

/// `lightspeed config diff`: exit status 0 when nothing would change, 2 when
/// something would.
async fn diff(args: DocumentArgs) -> Result<()> {
    let plans = plan_all(&args.files).await?;
    report(&plans, args.json)?;
    if plans.iter().any(DocumentPlan::has_conflicts) {
        bail!("the documents ask for changes apply will not make");
    }
    if plans.iter().any(DocumentPlan::has_changes) {
        std::process::exit(2);
    }
    Ok(())
}

/// `lightspeed config apply`: every document is planned before anything is
/// written, so a document that cannot be applied changes nothing.
async fn apply(args: DocumentArgs) -> Result<()> {
    let mut plans = plan_all(&args.files).await?;
    if plans.iter().any(DocumentPlan::has_conflicts) {
        report(&plans, args.json)?;
        bail!("the documents ask for changes apply will not make; nothing was written");
    }
    let mut outcome = Ok(());
    for plan in &mut plans {
        outcome = plan::apply(plan).await;
        if outcome.is_err() {
            break;
        }
    }
    report(&plans, args.json)?;
    outcome
}

/// `lightspeed config export`.
async fn export(args: ExportArgs) -> Result<()> {
    let base = base_connection();
    let mut exported = if args.deployment {
        export::deployment(&base).await?
    } else {
        let universe = base
            .active_universe()
            .context("no universe selected; name one with --universe, or pass --deployment")?;
        export::universe(&base, &universe).await?
    };
    let format = args.format.unwrap_or_else(|| {
        match args
            .output
            .as_deref()
            .and_then(|path| path.extension())
            .and_then(|extension| extension.to_str())
        {
            Some("json") => Format::Json,
            _ => Format::Yaml,
        }
    });
    if let (Some(profiles_dir), Some(output)) = (&args.profiles_dir, &args.output) {
        let document_dir = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        export::split_profiles(&mut exported, profiles_dir, document_dir)?;
    }
    let text = format.render(&exported)?;
    match &args.output {
        Some(output) => {
            if let Some(parent) = output
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            std::fs::write(output, text).with_context(|| format!("write {}", output.display()))?;
            eprintln!(
                "Wrote {} ({}). Secrets are named by placeholder sources; bind them before applying it elsewhere.",
                output.display(),
                format.extension()
            );
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn report(plans: &[DocumentPlan], json: bool) -> Result<()> {
    if json {
        let steps: Vec<&Step> = plans.iter().flat_map(|plan| &plan.steps).collect();
        println!("{}", serde_json::to_string_pretty(&steps)?);
        return Ok(());
    }
    for plan in plans {
        println!("{}", plan.subject);
        let mut unchanged = 0;
        for step in &plan.steps {
            if step.action == Action::Unchanged {
                unchanged += 1;
            } else {
                println!("{}", step.line());
            }
        }
        let changed = plan.steps.iter().filter(|step| step.done).count();
        let pending = plan
            .steps
            .iter()
            .filter(|step| !step.done && is_change(step))
            .count();
        println!("  {}", summary(changed, pending, unchanged));
    }
    Ok(())
}

fn is_change(step: &Step) -> bool {
    matches!(
        step.action,
        Action::Create | Action::Update | Action::Remove
    )
}

fn summary(changed: usize, pending: usize, unchanged: usize) -> String {
    let mut parts = Vec::new();
    if changed > 0 {
        parts.push(format!("{changed} changed"));
    }
    if pending > 0 {
        parts.push(format!("{pending} to change"));
    }
    parts.push(format!("{unchanged} unchanged"));
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_counts_what_was_and_what_would_be_changed() {
        assert_eq!(summary(0, 0, 4), "4 unchanged");
        assert_eq!(summary(0, 3, 4), "3 to change, 4 unchanged");
        assert_eq!(summary(2, 1, 0), "2 changed, 1 to change, 0 unchanged");
    }
}
