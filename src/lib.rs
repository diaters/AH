pub mod app;
pub mod channels;
pub mod contracts;
pub mod domain;
pub mod ecs;
pub mod infrastructure;
pub mod llm;
pub mod plugins;
pub mod systems;
pub mod triggers;
pub mod tui;
pub mod user_plugins;

// 精确 re-exports：仅在多个测试中复用、且属于稳定接口的类型。
// 不再使用模块级 glob（pub use xxx::*），保证类型来源单一可见
//（P0 依赖方向治理：docs/design/2026-08-17-complexity-governance-design.md）。
pub use crate::infrastructure::assets::{
    AgentAssetService, ExperienceAssetDraft, SkillPackageDraft,
};
pub use crate::systems::tools::NativeProcessBackend;

pub mod prelude {
    pub use bevy_app::prelude::*;
    pub use bevy_ecs::prelude::*;
    pub use bevy_time::prelude::*;
}
