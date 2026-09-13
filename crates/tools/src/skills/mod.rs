//! Skill discovery and catalog construction.

pub mod catalog;
pub(crate) mod catalog_text;
mod id;
pub mod model;
pub mod parser;
pub mod vfs;

pub use catalog::{
    SkillCatalogBuild, SkillCatalogBuilder, SkillCatalogError, SkillCatalogPublication,
    SkillCatalogRootInput, build_skill_catalog, build_skill_catalog_with_warnings,
    prepare_skill_catalog_publication, prepare_skill_catalog_publication_with_warnings,
    skill_catalog_context_input,
};
pub use id::SkillId;
pub use model::*;
pub use parser::{SkillFrontmatter, SkillParseError, parse_skill_frontmatter};
pub use vfs::{
    AttachedVfsSkillCatalogRoots, SkillVfsRootError, VfsSkillRootSpec,
    configured_vfs_skill_root_specs, resolve_attached_vfs_skill_roots,
};

pub mod environment;
