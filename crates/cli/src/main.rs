mod chat;

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
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Command::Chat(args) => chat::handle(args).await,
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
}
