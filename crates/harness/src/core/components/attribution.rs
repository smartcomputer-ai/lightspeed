use serde::{Deserialize, Serialize};

/// Who asked for a change, as the API boundary attributed the request: an
/// asserted actor, a configured token, a local caller, or the runtime's
/// own work. The harness records it and never branches on it; the wire shape
/// matches the public attribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Attribution {
    Actor {
        id: String,
    },
    /// The configured runtime token's name, never its secret.
    Token {
        name: String,
    },
    Local,
    Internal {
        component: String,
        cause: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_attribution_records_only_its_name() {
        let attribution = Attribution::Token {
            name: "operator".into(),
        };
        let wire = serde_json::json!({"kind": "token", "name": "operator"});
        assert_eq!(serde_json::to_value(&attribution).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Attribution>(wire).unwrap(),
            attribution
        );
    }
}
