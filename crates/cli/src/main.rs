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
}
