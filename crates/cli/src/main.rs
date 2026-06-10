mod api_client;
mod auth_cli;
mod chat;
mod mcp_cli;
mod skills_cli;
mod vfs_cli;
mod vfs_transfer;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "forge", version, about = "Forge command-line tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Chat through a Forge API gateway.
    Chat(chat::ChatArgs),
    /// Work with CAS-backed VFS snapshots.
    Vfs(vfs_cli::VfsArgs),
    /// List and manage session skills.
    Skills(skills_cli::SkillsArgs),
    /// Manage remote MCP servers and session links.
    Mcp(mcp_cli::McpArgs),
    /// Manage auth grants and credentials.
    Auth(auth_cli::AuthArgs),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_parse_accepts_model_and_workdir_options() {
        let cli = Cli::try_parse_from([
            "forge",
            "chat",
            "--new",
            "--provider",
            "openai",
            "--model",
            "gpt-5.5",
            "--effort",
            "medium",
            "--workdir",
            ".",
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
            "forge",
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
            "forge",
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
    fn vfs_snapshot_parse_accepts_directory_and_api_options() {
        let cli = Cli::try_parse_from([
            "forge",
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
            "forge",
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
            "forge",
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
            "forge",
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
            "forge",
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
            "forge",
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
            "forge",
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
            "forge",
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
            "--read-write",
        ])
        .expect("parse vfs mount put");
        assert!(matches!(cli.command, Command::Vfs(_)));
    }

    #[test]
    fn vfs_mount_delete_parse_accepts_session_and_path() {
        let cli = Cli::try_parse_from([
            "forge",
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
            "forge",
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
    fn skills_active_parse_accepts_json() {
        let cli = Cli::try_parse_from([
            "forge",
            "skills",
            "active",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "--json",
        ])
        .expect("parse skills active");
        assert!(matches!(cli.command, Command::Skills(_)));
    }

    #[test]
    fn skills_activate_parse_accepts_scope() {
        let cli = Cli::try_parse_from([
            "forge",
            "skills",
            "activate",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "--scope",
            "session",
            "skill:review",
        ])
        .expect("parse skills activate");
        assert!(matches!(cli.command, Command::Skills(_)));
    }

    #[test]
    fn skills_deactivate_parse_accepts_skill_id() {
        let cli = Cli::try_parse_from([
            "forge",
            "skills",
            "deactivate",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "skill:review",
        ])
        .expect("parse skills deactivate");
        assert!(matches!(cli.command, Command::Skills(_)));
    }

    #[test]
    fn mcp_server_add_parse_accepts_registry_options() {
        let cli = Cli::try_parse_from([
            "forge",
            "mcp",
            "server",
            "add",
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
        .expect("parse mcp server add");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn auth_grant_import_parse_accepts_token_env() {
        let cli = Cli::try_parse_from([
            "forge",
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
            "forge",
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
            "forge",
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
            "forge",
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
    fn mcp_server_add_parse_accepts_oauth_policy_metadata() {
        let cli = Cli::try_parse_from([
            "forge",
            "mcp",
            "server",
            "add",
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
        .expect("parse mcp server add with oauth policy");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }

    #[test]
    fn auth_login_parse_accepts_mcp_server_client_ids() {
        let cli = Cli::try_parse_from([
            "forge",
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
            "forge",
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
            "forge",
            "mcp",
            "link",
            "--api-url",
            "http://127.0.0.1:18080/rpc",
            "--session",
            "session_1",
            "--tool-id",
            "mcp_echo",
            "echo",
        ])
        .expect("parse mcp link");
        assert!(matches!(cli.command, Command::Mcp(_)));
    }
}
