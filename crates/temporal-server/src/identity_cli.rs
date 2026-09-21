//! Explicit host administration: possession of the database connection is the
//! trust boundary. Actor selection here is attribution, not remote login.

use std::path::PathBuf;

use access::{AccessScope, AccessStore};
use clap::Subcommand;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum IdentityCommand {
    /// Create the first deployment administrator; same-id retries are safe.
    Bootstrap {
        #[arg(long)]
        principal_id: Uuid,
        #[arg(long)]
        display_name: String,
    },
    /// Apply a typed core AccessChange JSON document as a named administrator.
    /// This local command requires trusted deployment database access.
    Apply {
        #[arg(long)]
        actor_principal: Uuid,
        #[arg(long)]
        file: PathBuf,
    },
    /// Inspect committed effective access; omitting universe selects deployment.
    Effective {
        #[arg(long)]
        principal_id: Uuid,
        #[arg(long)]
        universe_id: Option<Uuid>,
    },
    /// List only the universes in which this active principal has an assignment.
    Universes {
        #[arg(long)]
        principal_id: Uuid,
    },
}

pub fn now_ms() -> anyhow::Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

pub async fn run(command: IdentityCommand) -> anyhow::Result<()> {
    // No Temporal, model providers, secret master key, or Platform required.
    let pool = temporal_server::config::postgres_pool_from_env().await?;
    store_pg::verify_schema(&pool).await?;
    let store = store_pg::PgAccessStore::new(pool);
    let result = match command {
        IdentityCommand::Bootstrap {
            principal_id,
            display_name,
        } => serde_json::to_value(
            store
                .bootstrap(principal_id, display_name, now_ms()?)
                .await?,
        )?,
        IdentityCommand::Apply {
            actor_principal,
            file,
        } => {
            let change: access::AccessChange =
                serde_json::from_slice(&tokio::fs::read(file).await?)?;
            serde_json::to_value(store.apply(actor_principal, change, now_ms()?).await?)?
        }
        IdentityCommand::Effective {
            principal_id,
            universe_id,
        } => {
            let scope = universe_id.map_or(AccessScope::Deployment, |universe_id| {
                AccessScope::Universe { universe_id }
            });
            serde_json::to_value(store.effective_access(principal_id, scope).await?)?
        }
        IdentityCommand::Universes { principal_id } => {
            serde_json::to_value(store.accessible_universes(principal_id).await?)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Command, UniverseCommand};
    use clap::Parser;

    #[test]
    fn identity_and_universe_commands_require_explicit_principals() {
        assert!(
            Cli::try_parse_from(["server", "identity", "bootstrap", "--display-name", "Admin"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["server", "identity", "apply", "--file", "change.json"]).is_err()
        );
        assert!(Cli::try_parse_from(["server", "universe", "create"]).is_err());
        let id = "00000000-0000-0000-0000-000000000001";
        let cli = Cli::try_parse_from([
            "server",
            "identity",
            "bootstrap",
            "--principal-id",
            id,
            "--display-name",
            "Admin",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(Command::Identity(IdentityCommand::Bootstrap { principal_id, .. })) if principal_id == Uuid::from_u128(1))
        );
        let cli = Cli::try_parse_from(["server", "universe", "create", "--creator-principal", id])
            .unwrap();
        assert!(
            matches!(cli.command, Some(Command::Universe(UniverseCommand::Create { creator_principal, .. })) if creator_principal == Uuid::from_u128(1))
        );
    }
}
