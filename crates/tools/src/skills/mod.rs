//! Skill discovery and catalog construction.

pub mod activation;
pub mod catalog;
pub mod model;
pub mod parser;
pub mod vfs;

pub use activation::{SkillToolResultActivationInput, skill_activation_from_tool_result};
pub use catalog::{
    SkillCatalogBuild, SkillCatalogBuilder, SkillCatalogError, SkillCatalogPublication,
    SkillCatalogRootInput, build_skill_catalog, prepare_skill_catalog_publication,
};
pub use model::*;
pub use parser::{SkillFrontmatter, SkillParseError, parse_skill_frontmatter};
pub use vfs::{
    MountedVfsSkillCatalogRoots, SkillVfsRootError, VfsSkillRootSpec,
    conventional_vfs_skill_root_specs, resolve_mounted_vfs_skill_roots,
};
