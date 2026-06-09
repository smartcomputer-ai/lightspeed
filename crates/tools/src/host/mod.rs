//! Host interaction tool package.

pub mod apply_patch;
pub mod context;
pub mod executor;
pub mod fs;
pub mod process;
pub mod remote;
pub mod targets;
pub mod tools;

pub use context::{HostToolContext, HostToolLimits};
pub use executor::InlineHostToolRuntime;
pub use remote::{RemoteHostConnection, RemoteHostFileSystem, RemoteProcessExecutor};
pub use targets::{HOST_TARGET_NAMESPACE, HostToolTargets, LOCAL_HOST_TARGET_ID};
