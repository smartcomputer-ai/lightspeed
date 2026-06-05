//! Prompt instruction discovery and materialization.

pub mod assembler;
pub mod model;
pub mod vfs;

pub use assembler::{
    PromptInstructionsBuild, PromptInstructionsBuilder, PromptInstructionsError,
    PromptInstructionsPublication, PromptRootInput, active_prompt_instruction_entries,
    active_prompt_instruction_inputs, active_prompt_instruction_refs, build_prompt_instructions,
    prepare_prompt_instructions_publication, prompt_source_instructions_context_input,
};
pub use model::*;
pub use vfs::{
    MountedVfsPromptRoots, PromptVfsRootError, VfsPromptRootSpec,
    conventional_vfs_prompt_root_specs, resolve_mounted_vfs_prompt_roots,
};
