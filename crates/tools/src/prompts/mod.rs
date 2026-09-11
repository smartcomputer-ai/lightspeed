//! Prompt instruction discovery and materialization.

pub mod assembler;
pub mod environment;
pub mod model;
pub mod vfs;

pub use assembler::{
    PromptInstructionsBuild, PromptInstructionsBuilder, PromptInstructionsError,
    PromptInstructionsPublication, PromptRootInput, active_prompt_instruction_entries,
    active_prompt_instruction_inputs, active_prompt_instruction_refs, build_prompt_instructions,
    build_prompt_instructions_with_warnings, prepare_prompt_instructions_publication,
    prepare_prompt_instructions_publication_with_warnings,
    prompt_source_instructions_context_input,
};
pub use model::*;
pub use vfs::{
    LinkedVfsPromptRoots, PromptVfsRootError, VfsPromptRootSpec, configured_vfs_prompt_root_specs,
    conventional_vfs_prompt_root_specs, resolve_linked_vfs_prompt_roots,
};
