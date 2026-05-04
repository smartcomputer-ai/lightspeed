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
    /// Chat with a local in-process Forge agent.
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
            "hello",
        ])
        .expect("parse chat");
        assert!(matches!(cli.command, Command::Chat(_)));
    }

    #[test]
    fn chat_parse_accepts_prompt_options() {
        let cli =
            Cli::try_parse_from(["forge", "chat", "--new", "--prompt-profile", "local-coding"])
                .expect("parse prompt profile");
        assert!(matches!(cli.command, Command::Chat(_)));

        let cli = Cli::try_parse_from(["forge", "chat", "--new", "--prompt", "be concise"])
            .expect("parse inline prompt");
        assert!(matches!(cli.command, Command::Chat(_)));
    }
}
