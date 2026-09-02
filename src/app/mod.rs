use std::sync::Arc;

use crate::prelude::*;
use bevy_app::App;
use crossbeam_channel::Receiver;
use tokio::{runtime::Runtime, sync::mpsc};

use crate::{
    contracts::{AsyncRuntime, Clock, FrontendRegistry},
    domain::{
        AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentSpawnRequestMessage,
        Frontend, PendingKnowledgeWriteHooks, RetryReadyMessage, SharedKnowledgeBase, Signal, Task,
        TaskTerminatedMessage, ToolCallingState, UserInputMessage, UserOutputMessage,
    },
    llm::ExecutorRegistry,
    plugins::DefaultRuntimePluginGroup,
    systems::{
        HarnessSet, HarnessSettings, agent_factory_system, load_agents_system,
        validate_required_tags_system,
    },
};

/// 触发器配置路径 Resource：见 `domain::TriggersConfigPath`（装配期投影注入）。
pub use crate::domain::TriggersConfigPath;

pub fn build_harness_app(
    config: crate::systems::HarnessConfig,
    runtime: Arc<Runtime>,
    executor_registry: ExecutorRegistry,
    input_rx: Receiver<crate::domain::ExternalInput>,
    frontends: Vec<Box<dyn Frontend>>,
    channel_manager: crate::channels::ChannelManager,
) -> App {
    let (result_tx, result_rx) = mpsc::unbounded_channel();
    let (state_tx, state_rx) = mpsc::unbounded_channel();
    // 异步工具桥通道：dispatch 持 sender 投 worker，ingest 持 receiver 落地。
    // 双轨期 sync 工具不经此通道，但资源必须在 app 启动时装配，否则
    // `async_tool_dispatch_system` 注册后访问 `Res<ToolResultSender>` 会 panic。
    let (tool_result_tx, tool_result_rx) = mpsc::unbounded_channel();
    let mut app = App::new();

    // 基础 Resource
    app.insert_resource(crate::domain::InputReceiver(input_rx));
    app.insert_resource(FrontendRegistry { frontends });
    app.insert_resource(AsyncRuntime(runtime));
    app.insert_resource(executor_registry);
    app.insert_resource(crate::domain::ExecutionResultSender(result_tx));
    app.insert_resource(crate::domain::ExecutionResultReceiver(result_rx));
    app.insert_resource(crate::domain::ModelChainStateUpdateSender(state_tx));
    app.insert_resource(crate::domain::ModelChainStateUpdateReceiver(state_rx));
    app.insert_resource(crate::domain::ToolResultSender(tool_result_tx));
    app.insert_resource(crate::domain::ToolResultReceiver(tool_result_rx));
    app.insert_resource(TriggersConfigPath(config.triggers_config_path.clone()));
    // 记忆压缩配置：由 HarnessConfig（env 配置源）投影注入，
    // TaskRuntimePlugin::init_resource::<MemoryConfig>() 保留为默认兜底。
    app.insert_resource(config.memory.clone());
    app.insert_resource(HarnessSettings(config));
    app.insert_resource(Clock::default());
    app.insert_resource(crate::domain::ShutdownState::default());
    app.insert_resource(channel_manager);

    // 中心索引：外部 UUID 身份 → ECS Entity 映射。
    // 写入由 spawn/despawn 中心封装维护；陈旧兜底见下方 Maintenance 清理系统。
    app.init_resource::<crate::ecs::EntityIndex>();

    // Space Resources
    app.insert_resource(SharedKnowledgeBase::default());
    app.insert_resource(PendingKnowledgeWriteHooks::default());

    // Skill 加载器与注册表：由 brain_dispatch / experience_governance 等 system 通过 Res 读取。
    // build_registry 扫描 .harness/assets/agents/<owner>/skills/<name>/SKILL.md 构造 SkillRegistry。
    let skill_loader = crate::infrastructure::skills::SkillLoader::default_path();
    let skill_registry = skill_loader.build_registry();
    app.insert_resource(skill_loader);
    app.insert_resource(skill_registry);

    // Signal 触发路由（默认空，由 main.rs 根据 triggers.toml 配置覆盖）
    app.insert_resource(crate::domain::SignalTriggerRegistry::default());
    app.insert_resource(crate::triggers::SchedulerState::default());
    app.insert_resource(crate::triggers::SchedulerStateWatcher::default());
    app.insert_resource(crate::triggers::ScheduledTaskRegistry::default());

    // Startup: 先加载插件注册表，随后由 tools 系统主动拉取注册插件工具
    //（方向反转：user_plugins 不反向调用 systems），再加载持久化 Agent。
    app.add_systems(Startup, crate::user_plugins::plugin_load_startup_system);
    app.add_systems(
        Startup,
        crate::systems::tools::register_plugin_tools_startup_system
            .after(crate::user_plugins::plugin_load_startup_system),
    );
    app.add_systems(Startup, load_agents_system);

    // Configure SystemSets
    app.configure_sets(
        Update,
        (
            HarnessSet::Ingress,
            HarnessSet::Signal,
            HarnessSet::Transform,
            HarnessSet::Dispatch,
            HarnessSet::Execution,
            HarnessSet::Output,
            HarnessSet::Maintenance,
        )
            .chain(),
    );

    // 注册 PluginGroup
    app.add_plugins(DefaultRuntimePluginGroup);

    // agent_factory_system 在 HarnessSet::Maintenance 中运行，
    // agent_termination_system 和 experience_collection_cleanup_system
    // 由 ExecutionPlugin 在 HarnessSet::Execution 和 HarnessSet::Maintenance 中注册。
    // validate_required_tags_system 通过 Local<bool> 保证仅运行一次，扫描启动期
    // 已加载的持久化 Agent 与工具 required_tag 的匹配关系（O7）。
    app.add_systems(
        Update,
        (
            agent_factory_system.in_set(HarnessSet::Maintenance),
            validate_required_tags_system.in_set(HarnessSet::Maintenance),
        ),
    );

    // index 陈旧兜底清理（双保险之一）。
    // 即使绕过中心 despawn 封装直接 despawn，下一帧也能自动摘除映射。
    app.add_systems(
        Update,
        (
            crate::ecs::cleanup_index_on_task_remove,
            crate::ecs::cleanup_index_on_agent_remove,
        )
            .in_set(HarnessSet::Maintenance),
    );

    app
}

pub fn app_is_idle(world: &mut World) -> bool {
    let active_tasks = world
        .query::<&Task>()
        .iter(world)
        .filter(|task| !task.status.is_terminal())
        .count();
    let pending_signals = world.query::<&Signal>().iter(world).count();
    let pending_user_inputs = world.query::<&UserInputMessage>().iter(world).count();
    let pending_retry_ready = world.query::<&RetryReadyMessage>().iter(world).count();
    let pending_requests = world
        .query::<&AgentExecutionRequestMessage>()
        .iter(world)
        .count();
    let pending_results = world
        .query::<&AgentExecutionResultMessage>()
        .iter(world)
        .count();
    let pending_outputs = world.query::<&UserOutputMessage>().iter(world).count();
    let pending_spawn_requests = world
        .query::<&AgentSpawnRequestMessage>()
        .iter(world)
        .count();
    let pending_terminated = world.query::<&TaskTerminatedMessage>().iter(world).count();
    let pending_tool_calling = world.query::<&ToolCallingState>().iter(world).count();

    active_tasks == 0
        && pending_signals == 0
        && pending_user_inputs == 0
        && pending_retry_ready == 0
        && pending_requests == 0
        && pending_results == 0
        && pending_outputs == 0
        && pending_spawn_requests == 0
        && pending_terminated == 0
        && pending_tool_calling == 0
}
