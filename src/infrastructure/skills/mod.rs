pub mod diff;
pub mod loader;
pub mod registry;

pub use diff::{
    ALLOWED_FILE_SUFFIXES, ApplyError, FRONTMATTER_WHITELIST, apply_skill_operations,
    apply_skill_operations_multi, backup_skill_dir, cleanup_skill_dir_history,
    cleanup_skill_history, restore_skill_dir, validate_skill_file_path,
};
pub use loader::{
    LoadedSkill, PluginSkillContributions, PluginSkillEntry, SkillLoader, resolve_skill_closure,
};
pub use crate::domain::SkillId;
pub use registry::{SkillEntry, SkillRegistry};
