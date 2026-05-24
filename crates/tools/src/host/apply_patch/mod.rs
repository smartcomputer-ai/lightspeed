//! Apply-patch parser and engine.
//!
//! This module will hold the Forge-adapted Codex apply-patch implementation.
//! The canonical model-visible operation lives in `crate::host::tools::apply_patch`;
//! this module owns the lower-level parser, invocation detection, matching, and
//! filesystem application engine.

pub mod engine;
pub mod invocation;
pub mod parser;
pub mod seek_sequence;

pub use engine::{ApplyPatchSummary, apply_hunks, apply_patch_text};
pub use parser::{Hunk, ParseError, ParsedPatch, UpdateFileChunk, parse_patch};
