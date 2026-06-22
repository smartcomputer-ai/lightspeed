//! Apply-patch parser and filesystem application engine.
//!
//! The model-visible operation lives in `crate::fs::tools::apply_patch`; this
//! module owns the lower-level parser, invocation detection, matching, and
//! filesystem application engine.

pub mod engine;
pub mod invocation;
pub mod parser;
pub mod seek_sequence;

pub use engine::{ApplyPatchSummary, apply_hunks, apply_patch_text};
pub use parser::{Hunk, ParseError, ParsedPatch, UpdateFileChunk, parse_patch};
