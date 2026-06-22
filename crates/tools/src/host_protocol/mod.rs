//! Host protocol adapters.

pub mod remote;

pub use remote::{RemoteHostConnection, RemoteHostFileSystem, RemoteProcessExecutor};
