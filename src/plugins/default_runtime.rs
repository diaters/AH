//! Default Runtime Plugin Group
//!
//! 提供默认的运行时插件组合。

use crate::prelude::*;
use bevy_app::PluginGroupBuilder;

use super::{
    DispatchPlugin, ExecutionPlugin, FrontendPlugin, MemoryPlugin, TaskRuntimePlugin,
    ToolRuntimePlugin,
};

/// 默认运行时 Plugin 组
///
/// 包含所有核心功能的 Plugin 组合。
pub struct DefaultRuntimePluginGroup;

impl PluginGroup for DefaultRuntimePluginGroup {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(FrontendPlugin)
            .add(TaskRuntimePlugin)
            .add(DispatchPlugin)
            .add(ExecutionPlugin)
            .add(ToolRuntimePlugin)
            .add(MemoryPlugin)
    }
}
