use crate::chat::protocol::{ChatPromptConfig, ChatPromptProfile, ChatToolMode};

pub(crate) const LOCAL_CODING_PROMPT: &str = "\
You are a Forge local coding agent.

Use the registered file tools to inspect, search, read, and edit files under the selected workspace root.
When the user asks you to inspect or modify workspace state, do the work in the same run by calling tools before giving the final answer.
Do not claim a command or file operation succeeded unless tool results show it.
Keep final answers concise and mention the files or commands that matter.";

pub(crate) fn selected_prompt_text(
    config: &ChatPromptConfig,
    tool_mode: ChatToolMode,
) -> Option<&str> {
    match config {
        ChatPromptConfig::Auto if matches!(tool_mode, ChatToolMode::LocalCoding) => {
            Some(LOCAL_CODING_PROMPT)
        }
        ChatPromptConfig::Auto | ChatPromptConfig::None => None,
        ChatPromptConfig::Profile(ChatPromptProfile::None) => None,
        ChatPromptConfig::Profile(ChatPromptProfile::LocalCoding) => Some(LOCAL_CODING_PROMPT),
        ChatPromptConfig::Inline(text) if text.trim().is_empty() => None,
        ChatPromptConfig::Inline(text) => Some(text.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prompt_selects_local_coding_only_for_local_coding_tools() {
        assert_eq!(
            selected_prompt_text(&ChatPromptConfig::Auto, ChatToolMode::LocalCoding),
            Some(LOCAL_CODING_PROMPT)
        );
        assert_eq!(
            selected_prompt_text(&ChatPromptConfig::Auto, ChatToolMode::Workspace),
            None
        );
    }
}
