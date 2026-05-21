//! JSON-RPC method names for the host controller plane.

pub const INITIALIZE_METHOD: &str = "controller/initialize";

pub const LIST_TARGETS_METHOD: &str = "controller/listTargets";
pub const CREATE_TARGET_METHOD: &str = "controller/createTarget";
pub const ATTACH_TARGET_METHOD: &str = "controller/attachTarget";
pub const GET_TARGET_METHOD: &str = "controller/getTarget";
pub const CLOSE_TARGET_METHOD: &str = "controller/closeTarget";
