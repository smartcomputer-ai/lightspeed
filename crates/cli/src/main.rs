mod api_client;
mod chat;
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
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Command::Chat(args) => chat::handle(args).await,
        Command::Vfs(args) => vfs_cli::handle(args).await,
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
}
