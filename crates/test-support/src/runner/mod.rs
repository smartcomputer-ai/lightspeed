//! Test harness for driving CoreAgent sessions to quiescence.

mod drive;
mod error;
mod protocol;

pub use drive::SessionRunner;
pub use error::RunnerError;
pub use protocol::{
    DEFAULT_MAX_STEPS, DriveCommand, DriveOutcome, DriveSession, RunnerQuiescence, RunnerStores,
};
