//! The `message_*` tools a conversation carries into the bot's routed
//! session: bound to the conversation workflow as receiver, so a send needs
//! no route argument. Messages are named to the model by the bot's event
//! number (`#N`); provider message ids never reach the model.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use api::{
    BoundWorkflowToolDispatchInput, ToolParallelismView, WorkflowEndpointInput,
    WorkflowToolCompletionInput, WorkflowToolDeclarationInput, WorkflowToolDefinitionInput,
    WorkflowToolKindInput, WorkflowToolSpecInput, WorkflowToolTargetInput,
};
use serde_json::{Value, json};

pub const CHANNEL_TOOL_DEADLINE_MS: u64 = 120_000;

/// Declaration revision of the `message_*` tools. Declarations are
/// immutable per session; the bot controller rotates a routed session whose
/// carried declarations no longer match, so a bump here rolls out on the
/// next message. Bumped once when the declarations moved to the core.
pub const CHANNEL_TOOLS_REVISION: u32 = 3;

pub const CHANNEL_SEND_TOOL_ID: &str = "channels.message_send.v1";
pub const CHANNEL_EDIT_TOOL_ID: &str = "channels.message_edit.v1";
pub const CHANNEL_REACT_TOOL_ID: &str = "channels.message_react.v1";
pub const CHANNEL_NOOP_TOOL_ID: &str = "channels.message_noop.v1";
pub const CHANNEL_TOOL_IDS: [&str; 4] = [
    CHANNEL_SEND_TOOL_ID,
    CHANNEL_EDIT_TOOL_ID,
    CHANNEL_REACT_TOOL_ID,
    CHANNEL_NOOP_TOOL_ID,
];

pub const CHANNEL_SEND_DESCRIPTION: &str = "Send a message to this conversation. replyTo is the number of the message to reply to (the #N in its event header, or the number a previous send returned), or null. Completes after the provider acknowledges delivery and returns the new message's number.";
pub const CHANNEL_EDIT_DESCRIPTION: &str = "Edit a message you sent earlier in this conversation, by the number your send returned. Completes after the provider acknowledges the edit.";
pub const CHANNEL_REACT_DESCRIPTION: &str = "React to a message in this conversation by its number (the #N in its event header, or the number a send returned). Completes after the provider acknowledges the reaction.";
pub const CHANNEL_NOOP_DESCRIPTION: &str = "Deliberately send no reply to the conversation.";

/// Description blobs by name (`send`, `edit`, `react`, `noop`).
pub const CHANNEL_TOOL_DESCRIPTIONS: [(&str, &str); 4] = [
    ("send", CHANNEL_SEND_DESCRIPTION),
    ("edit", CHANNEL_EDIT_DESCRIPTION),
    ("react", CHANNEL_REACT_DESCRIPTION),
    ("noop", CHANNEL_NOOP_DESCRIPTION),
];

pub const CHANNEL_TOOL_SCHEMA_NAMES: [&str; 6] = [
    "sendInput",
    "editInput",
    "reactInput",
    "noopInput",
    "sendReceipt",
    "messageReceipt",
];

/// Input and receipt schemas by name, stored as blobs and referenced by
/// the declarations. Strict function schemas must require every declared
/// property, so optional values are required nullable properties.
pub static CHANNEL_TOOL_SCHEMAS: LazyLock<BTreeMap<&'static str, Value>> = LazyLock::new(|| {
    BTreeMap::from([
        (
            "sendInput",
            json!({
                "type": "object",
                "description": "Send a message to this conversation.",
                "properties": {
                    "text": { "type": "string", "minLength": 1, "description": "Message text in Markdown." },
                    "replyTo": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "Number of the message to reply to, or null.",
                    },
                },
                "required": ["text", "replyTo"],
                "additionalProperties": false,
            }),
        ),
        (
            "editInput",
            json!({
                "type": "object",
                "description": "Edit a message you sent earlier.",
                "properties": {
                    "message": { "type": "integer", "minimum": 1, "description": "Number returned by the send." },
                    "text": { "type": "string", "minLength": 1, "description": "Replacement text in Markdown." },
                },
                "required": ["message", "text"],
                "additionalProperties": false,
            }),
        ),
        (
            "reactInput",
            json!({
                "type": "object",
                "description": "React to a message.",
                "properties": {
                    "message": { "type": "integer", "minimum": 1, "description": "Number of the message." },
                    "emoji": { "type": "string", "minLength": 1 },
                },
                "required": ["message", "emoji"],
                "additionalProperties": false,
            }),
        ),
        (
            "noopInput",
            json!({
                "type": "object",
                "description": "Deliberately send no reply to the conversation.",
                "properties": {
                    "reason": { "type": "string" },
                },
                "required": ["reason"],
                "additionalProperties": false,
            }),
        ),
        (
            "sendReceipt",
            json!({
                "type": "object",
                "properties": {
                    "sent": { "type": "integer", "minimum": 1, "description": "Number of the message just sent." },
                },
                "required": ["sent"],
                "additionalProperties": false,
            }),
        ),
        (
            "messageReceipt",
            json!({
                "type": "object",
                "properties": {
                    "message": { "type": "integer", "minimum": 1 },
                },
                "required": ["message"],
                "additionalProperties": false,
            }),
        ),
    ])
});

pub fn channel_tool_description(name: &str) -> Option<&'static str> {
    CHANNEL_TOOL_DESCRIPTIONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, description)| *description)
}

pub fn is_channel_tool_id(tool_id: &str) -> bool {
    CHANNEL_TOOL_IDS.contains(&tool_id)
}

/// The declarations a conversation carries into the bot's routed session.
/// `schema_refs` and `description_refs` are the blob refs of
/// [`CHANNEL_TOOL_SCHEMAS`] and [`CHANNEL_TOOL_DESCRIPTIONS`] by name; a
/// missing ref is a programming error and panics.
pub fn channel_workflow_tool_declarations(
    receiver: WorkflowEndpointInput,
    schema_refs: &BTreeMap<&str, String>,
    description_refs: &BTreeMap<&str, String>,
) -> Vec<WorkflowToolDeclarationInput> {
    let schema = |name: &str| {
        schema_refs
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("missing channel tool schema ref: {name}"))
    };
    let description = |name: &str| {
        description_refs
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("missing channel tool description ref: {name}"))
    };
    let function =
        |input_schema_ref: String, description_ref: String| WorkflowToolKindInput::Function {
            description_ref: Some(description_ref),
            input_schema_ref,
            output_schema_ref: None,
            strict: Some(true),
            provider_options_ref: None,
        };
    let joined = |tool_id: &str,
                  semantic_type: &str,
                  name: &str,
                  input_schema: &str,
                  description_name: &str,
                  reply_schema: &str| WorkflowToolDeclarationInput {
        definition: WorkflowToolDefinitionInput {
            workflow_tool_id: tool_id.to_owned(),
            schema_revision: CHANNEL_TOOLS_REVISION,
            semantic_type: semantic_type.to_owned(),
            tool: WorkflowToolSpecInput {
                name: name.to_owned(),
                kind: function(schema(input_schema), description(description_name)),
                parallelism: ToolParallelismView::Exclusive,
            },
        },
        target: WorkflowToolTargetInput::Bound {
            receiver: receiver.clone(),
            dispatch: BoundWorkflowToolDispatchInput::Push,
        },
        completion: WorkflowToolCompletionInput::Joined {
            reply_schema_ref: Some(schema(reply_schema)),
            deadline_after_ms: Some(CHANNEL_TOOL_DEADLINE_MS),
        },
    };

    vec![
        joined(
            CHANNEL_SEND_TOOL_ID,
            "channels.message.send.v1",
            "message_send",
            "sendInput",
            "send",
            "sendReceipt",
        ),
        joined(
            CHANNEL_EDIT_TOOL_ID,
            "channels.message.edit.v1",
            "message_edit",
            "editInput",
            "edit",
            "messageReceipt",
        ),
        joined(
            CHANNEL_REACT_TOOL_ID,
            "channels.message.react.v1",
            "message_react",
            "reactInput",
            "react",
            "messageReceipt",
        ),
        WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                workflow_tool_id: CHANNEL_NOOP_TOOL_ID.to_owned(),
                schema_revision: CHANNEL_TOOLS_REVISION,
                semantic_type: "channels.message.noop.v1".to_owned(),
                tool: WorkflowToolSpecInput {
                    name: "message_noop".to_owned(),
                    kind: function(schema("noopInput"), description("noop")),
                    parallelism: ToolParallelismView::Exclusive,
                },
            },
            target: WorkflowToolTargetInput::Bound {
                receiver,
                dispatch: BoundWorkflowToolDispatchInput::Pull,
            },
            completion: WorkflowToolCompletionInput::Accepted,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs() -> (
        BTreeMap<&'static str, String>,
        BTreeMap<&'static str, String>,
    ) {
        let schemas = CHANNEL_TOOL_SCHEMA_NAMES
            .iter()
            .map(|name| (*name, format!("blob:{name}")))
            .collect();
        let descriptions = CHANNEL_TOOL_DESCRIPTIONS
            .iter()
            .map(|(name, _)| (*name, format!("blob:description-{name}")))
            .collect();
        (schemas, descriptions)
    }

    fn receiver() -> WorkflowEndpointInput {
        WorkflowEndpointInput {
            workflow_id: "channels/one".to_owned(),
            workflow_kind: "ChannelConversationWorkflow".to_owned(),
        }
    }

    #[test]
    fn every_strict_tool_property_is_required() {
        for name in ["sendInput", "editInput", "reactInput", "noopInput"] {
            let schema = &CHANNEL_TOOL_SCHEMAS[name];
            let mut properties: Vec<&str> = schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            let mut required: Vec<&str> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect();
            properties.sort_unstable();
            required.sort_unstable();
            assert_eq!(properties, required, "{name}");
            assert_eq!(schema["additionalProperties"], false, "{name}");
        }
        assert_eq!(CHANNEL_TOOL_SCHEMAS.len(), CHANNEL_TOOL_SCHEMA_NAMES.len());
        for name in CHANNEL_TOOL_SCHEMA_NAMES {
            assert!(CHANNEL_TOOL_SCHEMAS.contains_key(name), "{name}");
        }
    }

    #[test]
    fn names_messages_by_number_in_both_directions() {
        let schemas = &*CHANNEL_TOOL_SCHEMAS;
        assert_eq!(
            schemas["sendInput"]["properties"]["replyTo"]["type"],
            json!(["integer", "null"])
        );
        assert_eq!(
            schemas["editInput"]["properties"]["message"]["type"],
            "integer"
        );
        assert_eq!(
            schemas["reactInput"]["properties"]["message"]["type"],
            "integer"
        );
        assert_eq!(
            schemas["sendReceipt"]["properties"]["sent"]["type"],
            "integer"
        );
        for (name, schema) in schemas.iter() {
            let text = schema.to_string().to_lowercase();
            assert!(!text.contains("messageid"), "{name}");
            assert!(!text.contains("provider message id"), "{name}");
        }
    }

    #[test]
    fn declares_joined_delivery_tools_bound_to_the_conversation_and_an_accepted_noop() {
        let (schemas, descriptions) = refs();
        let tools = channel_workflow_tool_declarations(receiver(), &schemas, &descriptions);
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.definition.tool.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "message_send",
                "message_edit",
                "message_react",
                "message_noop"
            ]
        );
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.definition.workflow_tool_id.as_str())
                .collect::<Vec<_>>(),
            CHANNEL_TOOL_IDS
        );
        for tool in &tools {
            assert_eq!(tool.definition.schema_revision, CHANNEL_TOOLS_REVISION);
            assert_eq!(
                tool.definition.tool.parallelism,
                ToolParallelismView::Exclusive
            );
            let WorkflowToolKindInput::Function { strict, .. } = &tool.definition.tool.kind;
            assert_eq!(*strict, Some(true));
        }
        for tool in &tools[..3] {
            assert_eq!(
                tool.target,
                WorkflowToolTargetInput::Bound {
                    receiver: receiver(),
                    dispatch: BoundWorkflowToolDispatchInput::Push,
                }
            );
            let WorkflowToolCompletionInput::Joined {
                deadline_after_ms, ..
            } = &tool.completion
            else {
                panic!("delivery tools are joined");
            };
            assert_eq!(*deadline_after_ms, Some(CHANNEL_TOOL_DEADLINE_MS));
        }
        let reply_ref = |tool: &WorkflowToolDeclarationInput| match &tool.completion {
            WorkflowToolCompletionInput::Joined {
                reply_schema_ref, ..
            } => reply_schema_ref.clone(),
            _ => None,
        };
        assert_eq!(reply_ref(&tools[0]).as_deref(), Some("blob:sendReceipt"));
        assert_eq!(reply_ref(&tools[1]).as_deref(), Some("blob:messageReceipt"));
        assert_eq!(reply_ref(&tools[2]).as_deref(), Some("blob:messageReceipt"));
        let description_ref = |tool: &WorkflowToolDeclarationInput| {
            let WorkflowToolKindInput::Function {
                description_ref, ..
            } = &tool.definition.tool.kind;
            description_ref.clone().unwrap()
        };
        assert_eq!(
            tools.iter().map(description_ref).collect::<Vec<_>>(),
            vec![
                "blob:description-send",
                "blob:description-edit",
                "blob:description-react",
                "blob:description-noop",
            ]
        );
        let input_ref = |tool: &WorkflowToolDeclarationInput| {
            let WorkflowToolKindInput::Function {
                input_schema_ref, ..
            } = &tool.definition.tool.kind;
            input_schema_ref.clone()
        };
        assert_eq!(
            tools.iter().map(input_ref).collect::<Vec<_>>(),
            vec![
                "blob:sendInput",
                "blob:editInput",
                "blob:reactInput",
                "blob:noopInput"
            ]
        );
        assert_eq!(
            tools[3].target,
            WorkflowToolTargetInput::Bound {
                receiver: receiver(),
                dispatch: BoundWorkflowToolDispatchInput::Pull,
            }
        );
        assert_eq!(tools[3].completion, WorkflowToolCompletionInput::Accepted);
        let json = serde_json::to_value(&tools[0]).unwrap();
        assert_eq!(json["target"]["dispatch"], "push");
        assert_eq!(json["completion"]["type"], "joined");
    }

    #[test]
    fn does_not_instruct_the_model_to_await_joined_tools() {
        for description in [
            CHANNEL_SEND_DESCRIPTION,
            CHANNEL_EDIT_DESCRIPTION,
            CHANNEL_REACT_DESCRIPTION,
        ] {
            let lower = description.to_lowercase();
            assert!(!lower.contains("promise"), "{description}");
            assert!(
                !lower
                    .split(|ch: char| !ch.is_alphanumeric())
                    .any(|word| word == "await")
            );
            assert!(lower.contains("completes after the provider acknowledges"));
        }
        assert_eq!(
            channel_tool_description("noop"),
            Some(CHANNEL_NOOP_DESCRIPTION)
        );
        assert_eq!(channel_tool_description("nope"), None);
    }

    #[test]
    fn recognizes_channel_tool_ids() {
        assert!(is_channel_tool_id(CHANNEL_SEND_TOOL_ID));
        assert!(is_channel_tool_id(CHANNEL_NOOP_TOOL_ID));
        assert!(!is_channel_tool_id("bots.emit.v1"));
    }

    #[test]
    #[should_panic(expected = "missing channel tool schema ref: sendInput")]
    fn a_missing_schema_ref_is_a_programming_error() {
        let (_, descriptions) = refs();
        channel_workflow_tool_declarations(receiver(), &BTreeMap::new(), &descriptions);
    }
}
