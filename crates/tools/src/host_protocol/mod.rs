//! Host protocol adapters.

pub mod conformance;
pub mod remote;

pub use conformance::{
    HostDataConformanceError, HostDataConformanceOptions, assert_host_data_conformance,
};
pub use remote::{RemoteHostConnection, RemoteHostFileSystem, RemoteProcessExecutor};
