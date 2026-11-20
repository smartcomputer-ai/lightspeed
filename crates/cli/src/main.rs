mod administration_cli;
mod api_client;
mod chat;
mod config;
mod connection;
mod credentials_cli;
mod device_login;
mod env_cli;
mod key_secrets;
mod mcp_cli;
mod model_defaults_cli;
mod output;
mod platform_api;
mod platform_cli;
mod profile_cli;
mod request_headers;
mod session_cli;
mod session_resources;
mod skills_cli;
#[cfg(test)]
mod test_http;
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
    /// Deployment URL override; never forwards a saved key to a different endpoint.
    #[arg(long, global = true)]
    api_url: Option<String>,
    /// Read the bearer token from this file on every request (or set LIGHTSPEED_API_TOKEN_FILE).
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,
    /// Use a saved runtime or Platform connection for this invocation.
    #[arg(long, global = true)]
    connection: Option<String>,
    /// Platform organization for this invocation.
    #[arg(long, global = true)]
    organization: Option<String>,
    /// Universe UUID or handle for this invocation.
    #[arg(long, global = true)]
    universe: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage saved runtime and Platform connections.
    #[command(visible_alias = "connections")]
    Connect(connection::ConnectArgs),
    /// Manage Platform organizations, service users, and API keys.
    Platform(platform_cli::PlatformArgs),
    /// Call a runtime or Platform operation using JSON parameters.
    Api(platform_cli::ApiArgs),
    /// Select and administer runtime universes.
    #[command(visible_alias = "universes")]
    Universe(administration_cli::UniverseArgs),
    /// Generate a runtime token offline.
    Token(administration_cli::TokenArgs),
    /// Keep universe configuration in files: export it, check what a file
    /// would change, and apply it.
    Config(config::ConfigArgs),
    /// Discover models and configure model-provider connections.
    #[command(name = "model", visible_alias = "models")]
    Models(administration_cli::ModelsArgs),
    /// Chat with an agent session.
    #[command(visible_alias = "chats")]
    Chat(chat::ChatArgs),
    /// Manage persistent VFS workspaces in the current universe, and the
    /// snapshots they point at.
    #[command(visible_alias = "workspaces")]
    Workspace(vfs_cli::WorkspaceArgs),
    /// Manage registered MCP servers and their credentials.
    #[command(visible_alias = "mcps")]
    Mcp(mcp_cli::McpArgs),
    /// Manage reusable external credentials, OAuth clients and GitHub Apps.
    #[command(name = "credential", visible_alias = "credentials")]
    Credentials(credentials_cli::CredentialArgs),
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
    let mut cli = Cli::parse();
    if let Command::Connect(args) = cli.command {
        return connection::handle(
            args,
            cli.connection.as_deref(),
            cli.universe.as_deref(),
            cli.api_url.as_deref(),
            cli.organization.as_deref(),
            cli.token_file.as_deref(),
        )
        .await;
    }
    // Some commands are offline: they need no connection and must work
    // before a runtime exists.
    let offline = match &cli.command {
        Command::Token(_) => true,
        Command::Env(args) => args.offline(),
        Command::Config(args) => args.offline(),
        Command::Platform(args) => args.offline(),
        _ => false,
    };
    if offline {
        return match cli.command {
            Command::Token(args) => administration_cli::token(args),
            Command::Env(args) => env_cli::handle(args).await,
            Command::Config(args) => config::handle(args).await,
            Command::Platform(args) => platform_cli::handle(args).await,
            _ => unreachable!("only these commands have offline forms"),
        };
    }
    let enrollment = match &mut cli.command {
        Command::Config(args) => args.prepare_enrollment()?,
        Command::Platform(args) => args.enrollment(),
        _ => None,
    };
    let configured = connection::configured(
        cli.connection.as_deref(),
        cli.api_url.as_deref(),
        cli.universe.as_deref(),
        cli.token_file.as_deref(),
    )?;
    let wait = match &cli.command {
        Command::Config(args) => args.wait_seconds().map(|seconds| {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
            (deadline, seconds)
        }),
        _ => None,
    };
    let mut enrolled_organization = None;
    if let Some(params) = enrollment {
        let client = api_client::HttpAgentApi::with_connection(configured.clone());
        let result = connection::wait_for_startup(wait, || client.enroll(&params)).await?;
        if matches!(cli.command, Command::Platform(_)) {
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        let id = result["organizationId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("enrollment response omitted organization ID"))?;
        if result["enrolled"] == true {
            eprintln!("Enrolled organization {id}");
        }
        enrolled_organization = Some(id.to_owned());
    }
    let mut active =
        connection::wait_for_startup(wait, || connection::verify(configured.clone())).await?;
    connection::apply_organization_override(
        &mut active,
        cli.organization
            .as_deref()
            .or(enrolled_organization.as_deref()),
        cli.universe.as_deref(),
    )?;
    connection::ACTIVE
        .set(active)
        .map_err(|_| anyhow::anyhow!("connection already initialized"))?;
    match cli.command {
        Command::Connect(_) => unreachable!(),
        Command::Platform(args) => platform_cli::handle(args).await,
        Command::Api(args) => platform_cli::api(args).await,
        Command::Universe(args) => administration_cli::universe(args).await,
        Command::Config(args) => config::handle(args).await,
        Command::Token(args) => administration_cli::token(args),
        Command::Models(args) => administration_cli::models(args).await,
        Command::Chat(args) => chat::handle(args).await,
        Command::Workspace(args) => vfs_cli::workspace(args).await,
        Command::Mcp(args) => mcp_cli::handle(args).await,
        Command::Credentials(args) => credentials_cli::handle(args).await,
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
            ("model", "models"),
            ("chat", "chats"),
            ("workspace", "workspaces"),
            ("mcp", "mcps"),
            ("credential", "credentials"),
            ("environment", "environments"),
            ("profile", "profiles"),
            ("session", "sessions"),
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
        for removed in [
            "auth",
            "skills",
            "login",
            "logout",
            "organization",
            "service-user",
            "api-key",
            "access",
        ] {
            assert!(Cli::try_parse_from(["lightspeed", removed]).is_err());
        }
        for path in [
            vec!["connect", "login"],
            vec!["connect", "logout"],
            vec!["platform", "organization", "show"],
            vec!["platform", "service-user", "create"],
            vec!["platform", "api-key", "create"],
            vec!["universe", "access", "put"],
            vec!["api"],
        ] {
            let error =
                Cli::try_parse_from(std::iter::once("lightspeed").chain(path).chain(["--help"]))
                    .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        }
    }

    #[test]
    fn current_resource_commands_accept_scoped_flags() {
        for args in [
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
                "--provider",
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
            vec![
                "environment",
                "provider",
                "put",
                "local",
                "--controller-endpoint",
                "wss://provider.example/control",
            ],
            vec!["environment", "binding", "put", "local"],
            vec!["environment", "template", "list", "--provider", "local"],
            vec!["profile", "--json", "read", "reviewer"],
            vec!["session", "restart", "s1"],
        ] {
            let mut command = vec![
                "lightspeed",
                "--connection",
                "test",
                "--universe",
                "one",
                "--api-url",
                "http://localhost/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
            "export",
            "support",
            "--output",
            "./support-profile.json",
        ])
        .expect("parse profiles export");
        assert!(matches!(cli.command, Command::Profiles(_)));

        let cli = Cli::try_parse_from([
            "lightspeed",
            "profiles",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "check",
            "-",
        ])
        .expect("parse profiles check");
        assert!(matches!(cli.command, Command::Profiles(_)));
    }

    #[test]
    fn snapshot_upload_parse_accepts_directory_and_api_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspace",
            "snapshot",
            "upload",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "--put-batch-bytes",
            "1048576",
            ".",
        ])
        .expect("parse snapshot upload");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn snapshot_download_parse_accepts_snapshot_ref_and_destination() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspaces",
            "snapshots",
            "download",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "./out",
        ])
        .expect("parse snapshot download");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn vfs_workspace_create_parse_accepts_snapshot_ref() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "workspace",
            "create",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "--id",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
            "workspace_1",
        ])
        .expect("parse vfs workspace delete");
        assert!(matches!(cli.command, Command::Workspace(_)));
    }

    #[test]
    fn env_list_parse_accepts_universe_scope() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
        ])
        .expect("parse env list");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn session_list_parse_accepts_repeated_metadata() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "session",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
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
                "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
            "local-host",
        ])
        .expect("parse env close");
        assert!(matches!(cli.command, Command::Env(_)));
    }

    #[test]
    fn env_power_update_and_paging_parse() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "power",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "local-host",
            "paused",
        ])
        .expect("parse env power");
        assert!(matches!(cli.command, Command::Env(_)));
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "update",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "local-host",
            "--display-name",
            "Build box",
            "--metadata",
            "team=platform",
        ])
        .expect("parse env update");
        assert!(matches!(cli.command, Command::Env(_)));
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "--include-closed",
            "--limit",
            "50",
            "--after",
            "env-1",
        ])
        .expect("parse env list paging");
        assert!(matches!(cli.command, Command::Env(_)));
        assert!(
            Cli::try_parse_from([
                "lightspeed",
                "env",
                "idle-policy",
                "--api-url",
                "http://127.0.0.1:18080/api/v1",
                "local-host",
            ])
            .is_err()
        );
    }

    #[test]
    fn env_registration_key_and_binding_parse() {
        for command in [
            vec![
                "registration-key",
                "create",
                "--display-name",
                "Pods",
                "--offline-close-after",
                "60m",
            ],
            vec![
                "registration-key",
                "update",
                "registration_key_1",
                "--display-name",
                "Pods",
                "--max-active",
                "3",
            ],
            vec!["registration-key", "rotate", "registration_key_1"],
            vec!["binding", "put", "incus"],
            vec!["binding", "delete", "incus"],
            vec![
                "provider",
                "put",
                "incus",
                "--controller-endpoint",
                "unix:///run/provider.sock",
                "--data-route-base",
                "ws://127.0.0.1:19090/routes",
                "--clear-credential",
            ],
        ] {
            let mut args = vec!["lightspeed", "env"];
            args.extend(command);
            let cli =
                Cli::try_parse_from(&args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
            assert!(matches!(cli.command, Command::Env(_)));
        }
        assert!(
            Cli::try_parse_from(["lightspeed", "env", "binding", "put", "incus", "--enable"])
                .is_err(),
            "a binding has no on/off state"
        );
        assert!(
            Cli::try_parse_from([
                "lightspeed",
                "env",
                "provider",
                "put",
                "incus",
                "--controller-endpoint",
                "wss://provider.example/control",
                "--credential-file",
                "token",
                "--clear-credential",
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
            "http://127.0.0.1:18080/api/v1",
            "local-host",
            "--env-name",
            "GITHUB_TOKEN",
            "--grant",
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
            "http://127.0.0.1:18080/api/v1",
            "local-host",
            "--env-name",
            "GITHUB_TOKEN",
            "--grant",
            "authgrant_repo",
            "--provider-id",
            "openai",
        ])
        .expect_err("reject multiple credential sources");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn env_credentials_bind_has_no_stored_secret_source() {
        let error = Cli::try_parse_from([
            "lightspeed",
            "env",
            "credentials",
            "bind",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "local-host",
            "--env-name",
            "GITHUB_TOKEN",
            "--secret-id",
            "secret_repo",
        ])
        .expect_err("a stored secret is not a binding source");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn env_credentials_list_parse_accepts_environment() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "env",
            "credentials",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
            "local-host",
            "--env-name",
            "GITHUB_TOKEN",
        ])
        .expect("parse env credentials unbind");
        assert!(matches!(cli.command, Command::Env(_)));
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
            "http://127.0.0.1:18080/api/v1",
            "echo",
            "--label",
            "echo",
            "--allowed-tool",
            "hello",
            "--approval",
            "never",
            "--url",
            "https://echo.example.com/mcp",
        ])
        .expect("parse mcp server put");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn credential_client_add_parse_accepts_endpoints_and_secret_env() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "oauth-client",
            "create",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
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
        .expect("parse credential client add");
        assert!(matches!(cli.command, Command::Credentials(_)));
    }

    #[test]
    fn credential_client_add_rejects_multiple_secret_sources() {
        let result = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "oauth-client",
            "add",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
            "crm",
            "--label",
            "crm",
            "--auth-policy",
            "required-oauth",
            "--oauth-scope",
            "contacts.read",
            "--oauth-authorization-server",
            "https://as.example.com",
            "--url",
            "https://crm.example.com/mcp",
        ])
        .expect("parse mcp server put with oauth policy");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn credential_github_app_add_parse_requires_a_key_source() {
        let parsed = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "github",
            "app",
            "create",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "--id",
            "lightspeed-github",
            "--app-id",
            "12345",
            "--private-key-env",
            "GH_APP_KEY",
        ])
        .expect("parse github app create");
        assert!(matches!(parsed.command, Command::Credentials(_)));

        let missing_key = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "github",
            "app",
            "create",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "--app-id",
            "12345",
        ]);
        assert!(missing_key.is_err(), "a private key source is required");
    }

    #[test]
    fn credential_github_installation_grant_parse_accepts_app_and_id() {
        let parsed = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "github",
            "installation",
            "grant",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "--app",
            "lightspeed-github",
            "--installation-id",
            "678",
        ])
        .expect("parse installation grant");
        assert!(matches!(parsed.command, Command::Credentials(_)));
    }

    #[test]
    fn credential_login_parse_accepts_mcp_server_client_ids() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "login",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "mcp:crm",
        ])
        .expect("parse credential login mcp:");
        assert!(matches!(cli.command, Command::Credentials(_)));
    }

    #[test]
    fn credential_login_parse_accepts_client_and_overrides() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "credential",
            "login",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
            "crm",
            "--scope",
            "contacts.read",
            "--audience",
            "https://crm.example.com/mcp",
            "--no-wait",
        ])
        .expect("parse credential login");
        assert!(matches!(cli.command, Command::Credentials(_)));
    }

    #[test]
    fn mcp_server_auth_set_parse_accepts_server_and_grant() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "mcp",
            "credential",
            "set",
            "--api-url",
            "http://127.0.0.1:18080/api/v1",
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
            "http://127.0.0.1:18080/api/v1",
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
