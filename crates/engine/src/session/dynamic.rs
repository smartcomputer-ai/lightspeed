use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicCommand {
    pub kind: String,
    pub version: u32,
    pub payload: Value,
}

impl DynamicCommand {
    pub fn new(kind: impl Into<String>, version: u32, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            version,
            payload,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicEvent {
    pub kind: String,
    pub version: u32,
    pub payload: Value,
}

impl DynamicEvent {
    pub fn new(kind: impl Into<String>, version: u32, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            version,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn dynamic_event_round_trips_as_language_neutral_envelope() {
        let event = DynamicEvent::new(
            "lightspeed.run.started",
            1,
            json!({
                "run_id": 1,
                "input_ref": "blob:sha256:abc"
            }),
        );

        let value = serde_json::to_value(&event).expect("serialize event");

        assert_eq!(
            value,
            json!({
                "kind": "lightspeed.run.started",
                "version": 1,
                "payload": {
                    "run_id": 1,
                    "input_ref": "blob:sha256:abc"
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<DynamicEvent>(value).expect("deserialize event"),
            event
        );
    }
}
