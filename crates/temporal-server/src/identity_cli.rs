//! Explicit host administration: possession of the database connection is the
//! trust boundary. Actor selection here is attribution, not remote login.

use std::path::PathBuf;

use access::{AccessScope, AccessStore};
use clap::Subcommand;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum IdentityCommand {
    /// Explicit local development bootstrap; prints a new deployment key once.
    Development {
        #[arg(long)]
        universe_id: Uuid,
    },
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
    let store = store_pg::PgAccessStore::new(pool.clone());
    let result = match command {
        IdentityCommand::Development { universe_id } => {
            use auth::ApiKeyStore as _;
            store_pg::create_universe(&pool, universe_id).await?;
            let principal = store
                .initialize_local_development(universe_id, now_ms()?)
                .await?;
            // A real user is separate from the service credential in local Platform requests.
            let user_id = uuid::Uuid::from_u128(0x6c696768_7473_4065_8064_000000000002);
            for change in [
                access::AccessChange::CreatePrincipal {
                    id: user_id,
                    kind: access::PrincipalKind::User,
                    display_name: "Local administrator".into(),
                    management_scope: AccessScope::Deployment,
                },
                access::AccessChange::AssignRole {
                    assignment: access::RoleAssignment {
                        scope: AccessScope::Deployment,
                        subject: access::Subject::Principal(user_id),
                        role: access::Role::DeploymentAdmin,
                    },
                },
                access::AccessChange::AssignRole {
                    assignment: access::RoleAssignment {
                        scope: AccessScope::Universe { universe_id },
                        subject: access::Subject::Principal(user_id),
                        role: access::Role::Admin,
                    },
                },
                access::AccessChange::AssignCapability {
                    assignment: access::CapabilityAssignment {
                        scope: AccessScope::Deployment,
                        principal_id: principal.id,
                        capability: access::Capability::AssertUser,
                    },
                },
                access::AccessChange::AssignCapability {
                    assignment: access::CapabilityAssignment {
                        scope: AccessScope::Deployment,
                        principal_id: principal.id,
                        capability: access::Capability::ManageIdentity,
                    },
                },
            ] {
                store.apply(principal.id, change, now_ms()?).await?;
            }
            let keys = store_pg::PgApiKeyStore::new(pool);
            // The supervisor guarantees one local stack; retire credentials
            // from previous launcher runs without persisting their secrets.
            for previous in keys.list_api_keys().await? {
                if previous.principal_id == principal.id
                    && previous.display_name.as_deref() == Some("Local development launcher")
                    && previous.revoked_at_ms.is_none()
                {
                    keys.revoke_managed_key(
                        principal.id,
                        AccessScope::Deployment,
                        previous.scope,
                        &previous.key_prefix,
                        now_ms()?,
                    )
                    .await?;
                }
            }
            let key = auth::mint_api_key(
                AccessScope::Deployment,
                principal.id,
                principal.id,
                Some("Local development launcher".into()),
                now_ms()?,
            );
            keys.create_api_key(auth::CreateApiKey {
                authority_scope: access::AccessScope::Deployment,
                key_hash: key.key_hash,
                record: key.record,
            })
            .await?;
            serde_json::json!({"principalId":principal.id, "userPrincipalId":user_id, "secret":key.secret.expose()})
        }
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
    fn keys_require_explicit_issuer_principal_and_exactly_one_scope() {
        let id = "00000000-0000-4000-8000-000000000001";
        assert!(Cli::try_parse_from(["server", "api-key", "create", "--universe-id", id]).is_err());
        let base = [
            "server",
            "api-key",
            "create",
            "--principal",
            id,
            "--actor-principal",
            id,
        ];
        assert!(Cli::try_parse_from(base).is_err());
        assert!(Cli::try_parse_from(base.into_iter().chain(["--deployment"])).is_ok());
        assert!(
            Cli::try_parse_from(
                base.into_iter()
                    .chain(["--deployment", "--universe-id", id])
            )
            .is_err()
        );
        assert!(Cli::try_parse_from(["server", "api-key", "revoke", "lsk_example"]).is_err());
    }

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
