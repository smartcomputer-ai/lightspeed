//! Skill discovery and catalog construction.

pub mod catalog;
pub mod model;
pub mod parser;

pub use catalog::{
    SkillCatalogBuild, SkillCatalogBuilder, SkillCatalogError, SkillCatalogRootInput,
    build_skill_catalog,
};
pub use model::*;
pub use parser::{SkillFrontmatter, SkillParseError, parse_skill_frontmatter};
