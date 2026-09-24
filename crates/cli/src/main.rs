mod api_client;
mod auth_cli;
mod chat;
mod env_cli;
mod mcp_cli;
mod profile_cli;
mod session_cli;
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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Chat through a Lightspeed API gateway.
    Chat(chat::ChatArgs),
    /// Work with CAS-backed VFS snapshots.
    Vfs(vfs_cli::VfsArgs),
    /// List and manage session skills.
    Skills(skills_cli::SkillsArgs),
    /// Manage remote MCP servers and session links.
    Mcp(mcp_cli::McpArgs),
    /// Manage auth grants and credentials.
    Auth(auth_cli::AuthArgs),
    /// Manage session environments.
    Env(env_cli::EnvArgs),
    /// Manage agent profiles.
    Profiles(profile_cli::ProfilesArgs),
    /// Start, list, tag, close, and delete sessions.
    Session(session_cli::SessionArgs),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Command::Chat(args) => chat::handle(args).await,
        Command::Vfs(args) => vfs_cli::handle(args).await,
        Command::Skills(args) => skills_cli::handle(args).await,
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
    fn chat_parse_accepts_model_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "chat",
            "--new",
            "--provider",
            "openai",
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
    fn chat_parse_accepts_mount_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "chat",
            "--new",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--mount",
            ".",
            "--mount-path",
            "/workspace",
            "hello",
        ])
        .expect("parse chat mount");
        assert!(matches!(cli.command, Command::Chat(_)));
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
            "profiles",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "apply",
            "session_1",
            "--profile",
            "support",
        ])
        .expect("parse profiles apply");
        assert!(matches!(cli.command, Command::Profiles(_)));
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
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "materialize",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--workspace",
            "shared",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "./out",
        ])
        .expect("parse vfs materialize through a workspace");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_workspace_create_parse_accepts_snapshot_ref() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "workspace",
            "create",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--workspace-id",
            "workspace_1",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ])
        .expect("parse vfs workspace create");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_workspace_read_parse_accepts_workspace_id() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "workspace",
            "read",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "workspace_1",
        ])
        .expect("parse vfs workspace read");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_workspace_update_parse_accepts_expected_revision_and_snapshot_ref() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
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
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_workspace_update_parse_allows_omitted_expected_revision() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "workspace",
            "update",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "workspace_1",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ])
        .expect("parse vfs workspace update without expected revision");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_workspace_delete_parse_accepts_workspace_id() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "workspace",
            "delete",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "workspace_1",
        ])
        .expect("parse vfs workspace delete");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_mount_put_parse_accepts_workspace_mount() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "vfs",
            "mount",
            "put",
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
        assert!(matches!(cli.command, Command::Vfs(_)));
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
            "env",
            "activate",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "environment_1",
        ])
        .expect("parse env activate");
        assert!(matches!(cli.command, Command::Env(_)));
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
            "vfs",
            "mount",
            "delete",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "--path",
            "/workspace",
        ])
        .expect("parse vfs mount delete");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn skills_list_parse_accepts_session() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "skills",
            "list",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
        ])
        .expect("parse skills list");
        assert!(matches!(cli.command, Command::Skills(_)));
    }

    #[test]
    fn skills_use_parse_accepts_skill_id() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "skills",
            "use",
            "--api-url",
            "http://localhost:18080/rpc",
            "--session",
            "session_1",
            "--json",
            "skill:review",
        ])
        .expect("parse skill use");
        assert!(matches!(cli.command, Command::Skills(_)));
        for command in ["active", "activate", "deactivate"] {
            assert!(Cli::try_parse_from(["lightspeed", "skills", command]).is_err());
        }
    }

    #[test]
    fn mcp_server_put_parse_accepts_registry_options() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "mcp",
            "server",
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
            "auth",
            "grant",
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
            "auth",
            "grant",
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
            "auth",
            "client",
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
            "auth",
            "client",
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
            "server",
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
            "auth",
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
            "auth",
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
            "auth",
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
            "auth",
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
            "auth",
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
            "mcp",
            "link",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "echo",
        ])
        .expect("parse mcp link");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn mcp_server_auth_set_parse_accepts_server_and_grant() {
        let cli = Cli::try_parse_from([
            "lightspeed",
            "mcp",
            "server",
            "auth",
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
            "server",
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
