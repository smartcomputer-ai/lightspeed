//! Content projections and transcript rendering at provider boundaries.
//! Text views never rewrite the original JSON used for provider replay.

use serde_json::Value;

pub fn anthropic_text_blocks(blocks: &Value) -> Option<String> {
    blocks
        .as_array()?
        .iter()
        .map(|block| {
            if block.get("type")?.as_str()? != "text" {
                return None;
            }
            block.get("text")?.as_str().map(str::to_owned)
        })
        .collect()
}

pub fn openai_response_message(item: &Value) -> Option<String> {
    if item.get("type")?.as_str()? != "message" {
        return None;
    }
    let mut text = String::new();
    for part in item.get("content")?.as_array()? {
        match part.get("type")?.as_str()? {
            "output_text" => text.push_str(part.get("text")?.as_str()?),
            "refusal" => text.push_str(part.get("refusal")?.as_str()?),
            _ => {}
        }
    }
    Some(text)
}

pub const OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND: &str = "openai.completions.message";
pub const OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND: &str = "openai.completions.reasoning_state";
pub const OPENAI_RESPONSES_REASONING_PROVIDER_KIND: &str = "openai.responses.reasoning";
pub const ANTHROPIC_THINKING_PROVIDER_KIND: &str = "anthropic.messages.thinking";
/// Only provider-exposed text participates in display. Signatures and encrypted
/// continuation state remain in the payload for replay.
pub fn anthropic_thinking(raw: &Value) -> Option<String> {
    match raw.get("type")?.as_str()? {
        "thinking" => Some(
            raw.get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        "redacted_thinking" => Some(String::new()),
        _ => None,
    }
}

pub fn openai_response_reasoning(raw: &Value) -> Option<String> {
    if raw.get("type")?.as_str()? != "reasoning" {
        return None;
    }
    Some(
        ["summary", "content"]
            .into_iter()
            .flat_map(|key| raw.get(key).and_then(Value::as_array).into_iter().flatten())
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("summary_text" | "reasoning_text")
                )
            })
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn openai_completion_reasoning(raw: &Value) -> Option<String> {
    let object = raw.as_object()?;
    for key in ["reasoning_content", "reasoning"] {
        if let Some(text) = object
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            return Some(text.to_owned());
        }
    }
    Some(
        object
            .get("reasoning_details")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| {
                let field = match part.get("type")?.as_str()? {
                    "reasoning.text" => "text",
                    "reasoning.summary" => "summary",
                    _ => return None,
                };
                part.get(field)?.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn completion_refusal(
    message: &crate::openai::completions::CompletionMessage,
) -> Option<String> {
    use crate::openai::completions::CompletionMessageContent;
    message.refusal.clone().or_else(|| match &message.content {
        Some(CompletionMessageContent::Parts(parts)) => {
            let refusal = parts
                .iter()
                .filter_map(|part| part.refusal.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            (!refusal.is_empty()).then_some(refusal)
        }
        _ => None,
    })
}

pub fn openai_completion_message(raw: &Value) -> Option<String> {
    let message: crate::openai::completions::CompletionMessage =
        serde_json::from_value(raw.clone()).ok()?;
    if message.role != "assistant" {
        return None;
    }
    let text = message.text();
    Some(if text.is_empty() {
        completion_refusal(&message).unwrap_or_default()
    } else {
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_projection_excludes_encrypted_material() {
        assert_eq!(
            anthropic_thinking(
                &json!({"type":"thinking","thinking":"visible","signature":"secret"})
            )
            .as_deref(),
            Some("visible")
        );
        assert_eq!(
            anthropic_thinking(&json!({"type":"redacted_thinking","data":"secret"})).as_deref(),
            Some("")
        );
        assert_eq!(openai_response_reasoning(&json!({"type":"reasoning","encrypted_content":"secret","summary":[{"type":"summary_text","text":"visible"}],"content":[{"type":"encrypted","text":"secret"}]})).as_deref(), Some("visible"));
        assert_eq!(
            openai_response_reasoning(&json!({"type":"reasoning","encrypted_content":"secret"}))
                .as_deref(),
            Some("")
        );
        let details = json!({"reasoning_details":[{"type":"reasoning.text","text":"visible","signature":"secret"},{"type":"reasoning.summary","summary":"summary"},{"type":"reasoning.encrypted","data":"secret","text":"also secret"}]});
        assert_eq!(
            openai_completion_reasoning(&details).as_deref(),
            Some("visible\nsummary")
        );
        assert_eq!(
            openai_completion_reasoning(
                &json!({"reasoning_content":"visible","reasoning":"same text"})
            )
            .as_deref(),
            Some("visible")
        );
    }
}
