use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CodecError {
    #[error("unsupported envelope kind {kind:?} version {version}")]
    Unsupported { kind: String, version: u32 },

    #[error("codec failure: {message}")]
    Failed { message: String },
}
