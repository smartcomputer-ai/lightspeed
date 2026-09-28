mod administration_cli;
mod api_client;
mod auth_cli;
mod chat;
mod connection;
mod env_cli;
mod mcp_cli;
mod model_defaults_cli;
mod output;
mod profile_cli;
mod session_cli;
mod session_resources;
mod skills_cli;
mod vfs_cli;
mod vfs_transfer;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "lightspeed",
    version = release_info::LONG_VERSION,
    about = "Lightspeed command-line tools"
)]
struct Cli {
    /// Runtime endpoint override; never forwards a saved key to a different endpoint.
    #[arg(long, global = true)]
    api_url: Option<String>,
    /// Use a saved runtime connection for this invocation.
    #[arg(long, global = true)]
    connection: Option<String>,
    /// Universe UUID or slug for this invocation.
    #[arg(long, global = true)]
    universe: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage local runtime connections.
    #[command(visible_alias = "connections")]
    Connect(connection::ConnectArgs),
    /// Select and administer runtime universes.
    #[command(visible_alias = "universes")]
    Universe(administration_cli::UniverseArgs),
    /// Administer Lightspeed gateway API keys.
    #[command(visible_alias = "api-keys")]
    ApiKey(administration_cli::ApiKeyArgs),
    /// Discover models and configure model-provider connections.
    #[command(name = "model", visible_alias = "models")]
    Models(administration_cli::ModelsArgs),
    /// Chat with an agent session.
    #[command(visible_alias = "chats")]
    Chat(chat::ChatArgs),
    /// Upload and download immutable VFS snapshots.
    #[command(visible_alias = "vfses")]
    Vfs(vfs_cli::VfsArgs),
    /// Manage persistent VFS workspaces in the current universe.
    #[command(visible_alias = "workspaces")]
    Workspace(vfs_cli::WorkspaceArgs),
    /// Manage registered MCP servers and their credentials.
    #[command(visible_alias = "mcps")]
    Mcp(mcp_cli::McpArgs),
    /// Manage reusable external credentials, OAuth clients and GitHub Apps.
    #[command(name = "credential", visible_alias = "credentials")]
    Auth(auth_cli::AuthArgs),
    /// Provision and manage independent universe environments.
    #[command(name = "environment", visible_aliases = ["environments", "env"])]
    Env(env_cli::EnvArgs),
    /// Manage reusable agent profiles.
    #[command(name = "profile", visible_alias = "profiles")]
    Profiles(profile_cli::ProfilesArgs),
    /// Manage sessions, configuration, resource attachments and skills.
    #[command(visible_alias = "sessions")]
    Session(session_cli::SessionArgs),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    if let Command::Connect(args) = cli.command {
        return connection::handle(
            args,
            cli.connection.as_deref(),
            cli.universe.as_deref(),
            cli.api_url.as_deref(),
        )
        .await;
    }
    let active = connection::resolve(
        cli.connection.as_deref(),
        cli.api_url.as_deref(),
        cli.universe.as_deref(),
    )
    .await?;
    connection::ACTIVE
        .set(active)
        .map_err(|_| anyhow::anyhow!("connection already initialized"))?;
    match cli.command {
        Command::Connect(_) => unreachable!(),
        Command::Universe(args) => administration_cli::universe(args).await,
        Command::ApiKey(args) => administration_cli::api_key(args).await,
        Command::Models(args) => administration_cli::models(args).await,
        Command::Chat(args) => chat::handle(args).await,
        Command::Vfs(args) => vfs_cli::handle(args).await,
        Command::Workspace(args) => vfs_cli::workspace(args).await,
        Command::Mcp(args) => mcp_cli::handle(args).await,
        Command::Auth(args) => auth_cli::handle(args).await,
        Command::Env(args) => env_cli::handle(args).await,
        Command::Profiles(args) => profile_cli::handle(args).await,
        Command::Session(args) => session_cli::handle(args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_tree_and_plural_aliases_are_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        for (name, plural) in [
            ("connect", "connections"),
            ("universe", "universes"),
            ("api-key", "api-keys"),
            ("model", "models"),
            ("chat", "chats"),
            ("workspace", "workspaces"),
            ("mcp", "mcps"),
            ("credential", "credentials"),
            ("environment", "environments"),
            ("profile", "profiles"),
            ("session", "sessions"),
            ("vfs", "vfses"),
        ] {
            let root = Cli::command();
            let cmd = root.find_subcommand(name).unwrap();
            assert!(
                cmd.get_visible_aliases().any(|alias| alias == plural),
                "{name}"
            );
            let error = Cli::try_parse_from(["lightspeed", plural, "--help"]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        }
        for removed in ["auth", "skills"] {
            assert!(Cli::try_parse_from(["lightspeed", removed]).is_err());
        }
    }

    #[test]
    fn every_session_option_accepts_short_s() {
        use clap::CommandFactory;

        fn check(command: &clap::Command) {
            for arg in command.get_arguments() {
                if arg.get_long() == Some("session") {
                    assert_eq!(
                        arg.get_short(),
                        Some('s'),
                        "{} --session",
                        command.get_name()
                    );
                }
            }
            for subcommand in command.get_subcommands() {
                check(subcommand);
            }
        }
        let mut command = Cli::command();
        command.build();
        check(&command);
        for args in [
            vec!["chat", "-s", "test1"],
            vec!["session", "workspace", "list", "-s", "test1"],
            vec!["session", "mcp", "list", "-s", "test1"],
            vec!["session", "skill", "list", "-s", "test1"],
            vec!["session", "environment", "list", "-s", "test1"],
        ] {
            Cli::try_parse_from(std::iter::once("lightspeed").chain(args))
                .expect("-s must parse as --session");
        }
    }

    #[test]
    fn current_resource_commands_accept_scoped_flags() {
        for args in [
            vec![
                "session",
                "environment",
                "attach",
                "machine",
                "--session",
                "s1",
                "--access",
                "jobs",
                "--working-directory",
                "/repo",
            ],
            vec![
                "session",
                "environment",
                "detach",
                "machine",
                "--session",
                "s1",
            ],
            vec![
                "session",
                "config",
                "put",
                "s1",
                "--file",
                "config.json",
                "--expected-revision",
                "3",
            ],
            vec![
                "environment",
                "create",
                "--binding",
                "local",
                "--template",
                "linux-v1",
                "--request-id",
                "retry-1",
            ],
            vec![
                "environment",
                "register",
                "wss://env.example/ws",
                "--request-id",
                "retry-2",
            ],
            vec!["environment", "provider", "put", "--file", "provider.json"],
            vec!["environment", "binding", "put", "--file", "binding.json"],
            vec!["environment", "template", "list", "--binding", "local"],
            vec!["profile", "--json", "read", "reviewer"],
        ] {
            let mut command = vec![
                "lightspeed",
                "--connection",
                "test",
                "--universe",
                "one",
                "--api-url",
                "http://localhost/rpc",
            ];
            command.extend(args);
            Cli::try_parse_from(&command).unwrap_or_else(|error| panic!("{command:?}: {error}"));
        }
    }

    #[test]
    fn chat_parse_accepts_model_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "chat",
            "--new",
            "--provider",
            "openai",
            "--api-kind",
            "openai:responses",
            "--model",
            "gpt-5.5",
            "--effort",
            "medium",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "hello",
        ])
        .expect("parse chat");
        assert!(matches!(cli.command, Command::Chat(_)));
    }

    #[test]
    fn chat_parse_accepts_remote_api_url() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "chat",
            "--new",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "hello",
        ])
        .expect("parse api url");
        assert!(matches!(cli.command, Command::Chat(_)));
    }

    #[test]
    fn chat_stats_flag_parses_and_conflicts_with_listing() {
        assert!(Cli::try_parse_from(["lightspeed", "chat", "--show-stats"]).is_ok());
        assert!(Cli::try_parse_from(["lightspeed", "chat", "--show-stats", "hello"]).is_ok());
        assert!(Cli::try_parse_from(["lightspeed", "chat", "--list", "--show-stats"]).is_err());
    }

    #[test]
    fn chat_recent_session_flags_parse_and_reject_ambiguous_actions() {
        for flag in ["--list", "--resume", "--continue"] {
            assert!(Cli::try_parse_from(["lightspeed", "chat", flag]).is_ok());
        }
        assert!(Cli::try_parse_from(["lightspeed", "chat", "--list", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["lightspeed", "chat", "--continue", "hello"]).is_ok());
        for args in [
            vec!["--list", "--resume"],
            vec!["--list", "-s", "s1"],
            vec!["--list", "--new"],
            vec!["--list", "hello"],
            vec!["--list", "--upload", "."],
            vec!["--list", "--bare"],
            vec!["--continue", "--new"],
            vec!["--resume", "-s", "s1"],
            vec!["--resume", "--profile", "reviewer"],
            vec!["--new", "-s", "s1"],
        ] {
            assert!(Cli::try_parse_from(["lightspeed", "chat"].into_iter().chain(args)).is_err());
        }
    }

    #[test]
    fn chat_parse_accepts_workspace_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "chat",
            "--new",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--upload",
            ".",
            "--workspace-path",
            "/workspace",
            "--workspace-access",
            "read",
            "hello",
        ])
        .expect("parse chat upload");
        assert!(matches!(cli.command, Command::Chat(_)));
        assert!(Cli::try_parse_from(["lightspeed", "chat", "--workspace", "w1"]).is_ok());
        for args in [
            vec!["--upload", ".", "--workspace", "w1"],
            vec!["--workspace-path", "/repo"],
            vec!["--workspace-access", "read"],
            vec!["--workspace", "w1", "--workspace-access", "none"],
            vec!["--mount", "."],
            vec!["--mount-path", "/repo"],
            vec!["--filesystem-tools", "read"],
        ] {
            assert!(Cli::try_parse_from(["lightspeed", "chat"].into_iter().chain(args)).is_err());
        }
    }

    #[test]
    fn chat_parse_accepts_profile_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "chat",
            "--new",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--profile",
            "support",
            "hello",
        ])
        .expect("parse chat profile");
        assert!(matches!(cli.command, Command::Chat(_)));
    }

    #[test]
    fn profiles_parse_accepts_apply_named_profile() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "profile",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "apply",
            "session_1",
            "--profile",
            "support",
        ])
        .expect("parse profiles apply");
        assert!(matches!(cli.command, Command::Session(_)));
    }

    #[test]
    fn profiles_parse_accepts_import_export_and_check() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "profiles",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "import",
            "./support-profile.json",
            "--no-check",
        ])
        .expect("parse profiles import");
        assert!(matches!(cli.command, Command::Profiles(_)));

        let cli = Cli::try_parse_from([
            "lightspeed",
            "profiles",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "export",
            "support",
            "--out",
            "./support-profile.json",
        ])
        .expect("parse profiles export");
        assert!(matches!(cli.command, Command::Profiles(_)));

        let cli = Cli::try_parse_from([
            "lightspeed",
            "profiles",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "check",
            "-",
        ])
        .expect("parse profiles check");
        assert!(matches!(cli.command, Command::Profiles(_)));
    }

    #[test]
    fn vfs_snapshot_parse_accepts_directory_and_api_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "snapshot",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--put-batch-bytes",
            "1048576",
            ".",
        ])
        .expect("parse vfs snapshot");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_materialize_parse_accepts_snapshot_ref_and_destination() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "materialize",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "./out",
        ])
        .expect("parse vfs materialize");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_workspace_create_parse_accepts_snapshot_ref() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspace",
            "create",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--workspace-id",
            "workspace_1",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ])
        .expect("parse vfs workspace create");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn vfs_workspace_read_parse_accepts_workspace_id() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspace",
            "read",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "workspace_1",
        ])
        .expect("parse vfs workspace read");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn vfs_workspace_update_parse_accepts_expected_revision_and_snapshot_ref() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspace",
            "update",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--expected-revision",
            "4",
            "workspace_1",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ])
        .expect("parse vfs workspace update");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn vfs_workspace_update_parse_allows_omitted_expected_revision() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspace",
            "update",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "workspace_1",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ])
        .expect("parse vfs workspace update without expected revision");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn vfs_workspace_delete_parse_accepts_workspace_id() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspace",
            "delete",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "workspace_1",
        ])
        .expect("parse vfs workspace delete");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn vfs_mount_put_parse_accepts_workspace_mount() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "workspace",
            "attach",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "--path",
            "/workspace",
            "--workspace",
            "workspace_1",
            "--access",
            "edit",
        ])
        .expect("parse vfs mount put");
        assert!(matches!(cli.command, Command::Session(_)));
    }

    #[test]
    fn env_list_parse_accepts_universe_scope() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
        ])
        .expect("parse env list");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn env_activate_parse_accepts_environment() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "environment",
            "activate",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "environment_1",
        ])
        .expect("parse env activate");
        assert!(matches!(cli.command, Command::Session(_)));
    }

    #[test]
    fn session_list_parse_accepts_repeated_metadata() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--metadata",
            "source=harbor",
            "--metadata",
            "job=nightly",
        ])
        .expect("parse session list");
        assert!(matches!(cli.command, Command::Session(_)));
    }

    #[test]
    fn session_close_parse_rejects_malformed_metadata() {
        assert!(
            Cli::try_parse_from([
                "lightspeed",
                "session",
                "close",
                "--api-url",
                "http://127.0.0.1:18080/rpc",
                "--metadata",
                "novalue",
            ])
            .is_err()
        );
    }

    #[test]
    fn env_list_parse_accepts_metadata_filter() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--metadata",
            "harborContextId=abc",
        ])
        .expect("parse env list");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn env_close_parse_accepts_environment() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "close",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "local-host",
        ])
        .expect("parse env close");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn env_power_and_idle_policy_parse() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "power",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "local-host",
            "paused",
        ])
        .expect("parse env power");
        assert!(matches!(cli.command, Command::Env(_)));
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "idle-policy",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "local-host",
            "--pause-after-min",
            "15",
            "--close-after-min",
            "240",
        ])
        .expect("parse env idle-policy");
        assert!(matches!(cli.command, Command::Env(_)));
        assert!(
            Cli::try_parse_from([
                "lightspeed",
                "env",
                "idle-policy",
                "--api-url",
                "http://127.0.0.1:18080/rpc",
                "local-host",
                "--clear",
                "--pause-after-min",
                "15",
            ])
            .is_err()
        );
    }

    #[test]
    fn env_credentials_bind_parse_accepts_grant_source() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "credentials",
            "bind",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "local-host",
            "--env-name",
            "GITHUB_TOKEN",
            "--grant-id",
            "authgrant_repo",
        ])
        .expect("parse env credentials bind");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn env_credentials_bind_rejects_multiple_sources() {
        let error = Cli::try_parse_from([
            "lightspeed",
            "env",
            "credentials",
            "bind",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "local-host",
            "--env-name",
            "GITHUB_TOKEN",
            "--grant-id",
            "authgrant_repo",
            "--secret-id",
            "secret_repo",
        ])
        .expect_err("reject multiple credential sources");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn env_credentials_list_parse_accepts_environment() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "credentials",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "local-host",
        ])
        .expect("parse env credentials list");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn env_credentials_unbind_parse_accepts_env_name() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "credentials",
            "unbind",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "local-host",
            "--env-name",
            "GITHUB_TOKEN",
        ])
        .expect("parse env credentials unbind");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn vfs_mount_delete_parse_accepts_session_and_path() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "workspace",
            "detach",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "--path",
            "/workspace",
        ])
        .expect("parse vfs mount delete");
        assert!(matches!(cli.command, Command::Session(_)));
    }

    #[test]
    fn skills_list_parse_accepts_session() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "skill",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
        ])
        .expect("parse skills list");
        assert!(matches!(cli.command, Command::Session(_)));
    }

    #[test]
    fn skills_use_parse_accepts_skill_id() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "skill",
            "use",
            "--api-url",
            "http://localhost:18080/rpc",
            "--session",
            "session_1",
            "--json",
            "skill:review",
        ])
        .expect("parse skill use");
        assert!(matches!(cli.command, Command::Session(_)));
        for command in ["active", "activate", "deactivate"] {
            assert!(Cli::try_parse_from(["lightspeed", "session", "skill", command]).is_err());
        }
    }

    #[test]
    fn mcp_server_put_parse_accepts_registry_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "mcp",
            "put",
            "--expected-revision",
            "2",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--id",
            "echo",
            "--label",
            "echo",
            "--allowed-tool",
            "hello",
            "--approval",
            "never",
            "https://echo.example.com/mcp",
        ])
        .expect("parse mcp server put");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn auth_grant_import_parse_accepts_token_env() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "import",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--id",
            "authgrant_crm",
            "--token-env",
            "CRM_MCP_TOKEN",
            "--audience",
            "https://crm.example.com/mcp",
        ])
        .expect("parse auth grant import");
        assert!(matches!(cli.command, Command::Auth(_)));
    }

    #[test]
    fn auth_grant_import_requires_a_token_source() {
        let result = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "import",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
        ]);
        assert!(result.is_err(), "token source must be required");
    }

    #[test]
    fn auth_client_add_parse_accepts_endpoints_and_secret_env() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "oauth-client",
            "add",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--id",
            "crm",
            "--kind",
            "mcp-oauth",
            "--authorization-endpoint",
            "https://as.example.com/authorize",
            "--token-endpoint",
            "https://as.example.com/token",
            "--client-id",
            "client-1",
            "--client-secret-env",
            "CRM_OAUTH_CLIENT_SECRET",
            "--audience",
            "https://crm.example.com/mcp",
        ])
        .expect("parse auth client add");
        assert!(matches!(cli.command, Command::Auth(_)));
    }

    #[test]
    fn auth_client_add_rejects_multiple_secret_sources() {
        let result = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "oauth-client",
            "add",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--authorization-endpoint",
            "https://as.example.com/authorize",
            "--token-endpoint",
            "https://as.example.com/token",
            "--client-id",
            "client-1",
            "--client-secret",
            "s1",
            "--client-secret-env",
            "S2",
        ]);
        assert!(result.is_err(), "secret sources are mutually exclusive");
    }

    #[test]
    fn mcp_server_put_parse_accepts_oauth_policy_metadata() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "mcp",
            "put",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--id",
            "crm",
            "--label",
            "crm",
            "--auth-policy",
            "required-oauth",
            "--oauth-scope",
            "contacts.read",
            "--oauth-authorization-server",
            "https://as.example.com",
            "https://crm.example.com/mcp",
        ])
        .expect("parse mcp server put with oauth policy");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn auth_github_app_add_parse_requires_a_key_source() {
        let parsed = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "github",
            "app",
            "add",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--id",
            "lightspeed-github",
            "--app-id",
            "12345",
            "--private-key-env",
            "GH_APP_KEY",
        ])
        .expect("parse github app add");
        assert!(matches!(parsed.command, Command::Auth(_)));

        let missing_key = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "github",
            "app",
            "add",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--app-id",
            "12345",
        ]);
        assert!(missing_key.is_err(), "a private key source is required");
    }

    #[test]
    fn auth_github_installation_grant_parse_accepts_app_and_id() {
        let parsed = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "github",
            "installation",
            "grant",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--app",
            "lightspeed-github",
            "--installation-id",
            "678",
        ])
        .expect("parse installation grant");
        assert!(matches!(parsed.command, Command::Auth(_)));
    }

    #[test]
    fn auth_login_parse_accepts_mcp_server_client_ids() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "login",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "mcp:crm",
        ])
        .expect("parse auth login mcp:");
        assert!(matches!(cli.command, Command::Auth(_)));
    }

    #[test]
    fn auth_login_parse_accepts_client_and_overrides() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "login",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "crm",
            "--scope",
            "contacts.read",
            "--audience",
            "https://crm.example.com/mcp",
            "--no-wait",
        ])
        .expect("parse auth login");
        assert!(matches!(cli.command, Command::Auth(_)));
    }

    #[test]
    fn mcp_link_parse_accepts_session_and_server() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "mcp",
            "attach",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "echo",
        ])
        .expect("parse mcp link");
        assert!(matches!(cli.command, Command::Session(_)));
    }

    #[test]
    fn mcp_server_auth_set_parse_accepts_server_and_grant() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "mcp",
            "credential",
            "set",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "crm",
            "--grant",
            "authgrant_1",
        ])
        .expect("parse MCP server auth set");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn mcp_server_login_parse_accepts_server_and_oauth_overrides() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "mcp",
            "login",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "crm",
            "--scope",
            "contacts.read",
            "--audience",
            "https://crm.example.com/mcp",
        ])
        .expect("parse MCP server login");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }
}
