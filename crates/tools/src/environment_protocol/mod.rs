//! Environment protocol adapters.

pub mod conformance;
pub mod remote;

pub use conformance::{DataConformance, DataConformanceFailure, DataConformanceReport};
pub use remote::{
    RemoteEnvironmentConnection, RemoteEnvironmentFileSystem, RemoteJobExecutor,
    RemoteProcessExecutor,
};
