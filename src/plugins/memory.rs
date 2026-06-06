//! Memory Plugin
//!
//! 提供记忆管理相关的系统。

use bevy::prelude::*;

use crate::systems::{
    HarnessSet, init_agent_memory_system, memory_absorption_system, memory_compression_system,
    summarization_dispatch_system,
};

/// 记忆 Plugin
///
/// 负责记忆的压缩、贡献和初始化。
pub struct MemoryPlugin;

impl Plugin for MemoryPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                // 记忆压缩
                memory_compression_system.in_set(HarnessSet::Maintenance),
                // Agent 记忆初始化
                init_agent_memory_system.in_set(HarnessSet::Maintenance),
                // 记忆吸收
                memory_absorption_system.in_set(HarnessSet::Maintenance),
                // 摘要派发
                summarization_dispatch_system
                    .in_set(HarnessSet::Maintenance)
                    .after(crate::systems::agent_factory_system),
            ),
        );
    }
}
