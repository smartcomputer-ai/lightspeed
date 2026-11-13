use serde::{Deserialize, Serialize};

/// Who asked for a change, as the API boundary attributed the request: an
/// asserted actor, a key acting for itself, a local caller, or the runtime's
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
    /// A key acting for itself, named by its key id, which outlives the
    /// key's secret. A key id is unique among the keys of one scope, and a
    /// session belongs to one universe, so the scope kind completes the
    /// name.
    Key {
        key_id: String,
        scope_kind: AttributedKeyScope,
    },
    Local,
    Internal {
        component: String,
        cause: String,
    },
}

/// Whether an attributed key is a deployment key or a key of the session's
/// own universe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum AttributedKeyScope {
    Deployment,
    Universe,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_key_is_recorded_by_its_id_and_scope_kind() {
        for (scope_kind, spelling) in [
            (AttributedKeyScope::Deployment, "deployment"),
            (AttributedKeyScope::Universe, "universe"),
        ] {
            let attribution = Attribution::Key {
                key_id: "ci-bot".to_owned(),
                scope_kind,
            };
            let stored = json!({"kind": "key", "keyId": "ci-bot", "scopeKind": spelling});
            assert_eq!(serde_json::to_value(&attribution).unwrap(), stored);
            assert_eq!(
                serde_json::from_value::<Attribution>(stored).unwrap(),
                attribution
            );
        }
    }

    /// An event that names a key by the display prefix of its secret names
    /// no key. It must fail to decode, never load as some other attribution.
    #[test]
    fn a_key_named_by_its_secret_prefix_does_not_decode() {
        for stale in [
            json!({"kind": "key", "prefix": "lsk_abcdefgh"}),
            json!({"kind": "key", "keyId": "ci-bot"}),
            json!({"kind": "key", "keyId": "ci-bot", "scopeKind": "tenant"}),
        ] {
            assert!(
                serde_json::from_value::<Attribution>(stale.clone()).is_err(),
                "{stale}"
            );
        }
    }

    #[test]
    fn the_other_attributions_keep_their_recorded_shape() {
        for (attribution, stored) in [
            (
                Attribution::Actor {
                    id: "user-1".to_owned(),
                },
                json!({"kind": "actor", "id": "user-1"}),
            ),
            (Attribution::Local, json!({"kind": "local"})),
            (
                Attribution::Internal {
                    component: "controller".to_owned(),
                    cause: "bot".to_owned(),
                },
                json!({"kind": "internal", "component": "controller", "cause": "bot"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&attribution).unwrap(), stored);
            assert_eq!(
                serde_json::from_value::<Attribution>(stored).unwrap(),
                attribution
            );
        }
    }
}
