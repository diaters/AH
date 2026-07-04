//! Memory Plugin
//!
//! 提供记忆管理相关的系统。

use crate::prelude::*;

use crate::infrastructure::assets::AgentAssetService;
use crate::infrastructure::memory::LongTermMemoryService;
use crate::systems::{
    HarnessSet, init_agent_memory_system, long_term_memory_decay_system, memory_compression_system,
    on_ltm_evicted_hook_system, on_ltm_write_hook_system, summarization_dispatch_system,
};

/// 记忆 Plugin
///
/// 负责记忆的压缩、衰退治理、贡献和初始化。
pub struct MemoryPlugin;

impl Plugin for MemoryPlugin {
    fn build(&self, app: &mut App) {
        // 注册长期记忆服务 Resource
        app.insert_resource(LongTermMemoryService::default_json());
        app.insert_resource(AgentAssetService::default_path());
        app.insert_resource(
            crate::infrastructure::incubation::proposal_store::IncubationProposalStore::default_path(),
        );
        app.insert_resource(
            crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry,
        );

        app.add_systems(
            Update,
            (
                // 记忆压缩
                memory_compression_system.in_set(HarnessSet::Maintenance),
                // Agent 记忆初始化
                init_agent_memory_system.in_set(HarnessSet::Maintenance),
                // on_long_term_memory_write 观察 hook companion 系统
                on_ltm_write_hook_system
                    .in_set(HarnessSet::Maintenance)
                    .after(init_agent_memory_system),
                // 长期记忆衰退治理
                long_term_memory_decay_system.in_set(HarnessSet::Maintenance),
                // on_long_term_memory_evicted 观察 hook companion 系统
                on_ltm_evicted_hook_system
                    .in_set(HarnessSet::Maintenance)
                    .after(long_term_memory_decay_system),
                // 摘要派发
                summarization_dispatch_system
                    .in_set(HarnessSet::Maintenance)
                    .after(crate::systems::agent_factory_system),
            ),
        );
    }
}
