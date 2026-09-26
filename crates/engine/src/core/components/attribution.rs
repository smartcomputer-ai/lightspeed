use serde::{Deserialize, Serialize};

/// Who asked for a change, as the API boundary attributed the request: an
/// asserted actor, a key acting for itself, a local caller, or the runtime's
/// own work. The engine records it and never branches on it; the wire shape
/// matches the public attribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Attribution {
    Actor { id: String },
    Key { prefix: String },
    Local,
    Internal { component: String, cause: String },
}
