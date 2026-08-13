//! JSON-RPC method names for the host controller plane.

pub const INITIALIZE_METHOD: &str = "controller/initialize";

pub const LIST_TEMPLATES_METHOD: &str = "controller/listTemplates";
pub const LIST_TARGETS_METHOD: &str = "controller/listTargets";
pub const CREATE_TARGET_METHOD: &str = "controller/createTarget";
pub const ADOPT_TARGET_METHOD: &str = "controller/adoptTarget";
pub const GET_TARGET_METHOD: &str = "controller/getTarget";
pub const CLOSE_TARGET_METHOD: &str = "controller/closeTarget";
pub const ENSURE_INGRESS_METHOD: &str = "controller/ensureIngress";
pub const REMOVE_INGRESS_METHOD: &str = "controller/removeIngress";
