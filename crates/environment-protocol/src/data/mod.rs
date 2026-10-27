//! The data plane: one envd serving one environment's filesystem,
//! processes and jobs. The runtime is the client; envd is the server.

pub mod exec;
pub mod fs;
pub mod handshake;
pub mod idle;
pub mod job;
pub mod process;
pub mod scan;
pub mod transfer;
