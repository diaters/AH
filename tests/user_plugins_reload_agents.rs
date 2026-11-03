//! 集成测试：`/reload-plugins` 后插件贡献的 Agent 按最新插件目录重新生成。
//!
//! 重载会清除旧插件贡献的 Agent 实体；修复前重载后不再生成，插件 Agent 永久消失。

use std::path::Path;
use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use common::mock_executor::EchoExecutor;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentExecutor, domain::ReloadPluginsMessage,
    llm::ExecutorRegistry, systems::HarnessConfig,
};

mod common;

/// 进程内串行化 HARNESS_PLUGINS_DIR 访问的全局锁（`std::env::set_var` 非线程安全）。
static PLUGIN_ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_agents_config(path: &Path) {
    std::fs::write(
        path,
        r#"
[[agent]]
name = "main"
tags = ["general"]
description = "主 Agent"
"#,
    )
    .unwrap();
}

/// 写入贡献一个 Agent 的插件 `alpha`，profile 描述为 `description`。
fn write_agent_plugin(plugins_dir: &Path, description: &str) {
    let plugin_dir = plugins_dir.join("alpha");
    std::fs::create_dir_all(plugin_dir.join("agents")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[agents]]
id = "helper"
profile = "agents/helper.toml"
"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("agents/helper.toml"),
        format!(
            r#"
name = "helper"
tags = ["helper"]
description = "{description}"
"#
        ),
    )
    .unwrap();
}

/// 返回所有名为 `name` 的 Agent 的描述。
fn agent_descriptions(app: &mut bevy_app::App, name: &str) -> Vec<String> {
    let world = app.world_mut();
    let mut query = world.query::<&Agent>();
    query
        .iter(world)
        .filter(|a| a.profile.name == name)
        .map(|a| a.capabilities.description.clone())
        .collect()
}

fn reload_plugins(app: &mut bevy_app::App) {
    app.world_mut().spawn(ReloadPluginsMessage);
    for _ in 0..3 {
        app.update();
    }
}

#[test]
fn reload_plugins_respawns_plugin_agents_from_current_dir() {
    let config_dir = TempDir::new().unwrap();
    let agents_path = config_dir.path().join("agents.toml");
    write_agents_config(&agents_path);

    let plugins_dir = TempDir::new().unwrap();
    write_agent_plugin(plugins_dir.path(), "v1");

    let _env_guard = PLUGIN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: `PLUGIN_ENV_LOCK` 串行化本二进制中所有触碰此 env 的测试；
    // HARNESS_PLUGINS_DIR 指向的临时目录存活至本函数结束。
    unsafe {
        std::env::set_var("HARNESS_PLUGINS_DIR", plugins_dir.path());
    }

    let config = HarnessConfig {
        agents_config_path: agents_path.display().to_string(),
        ..HarnessConfig::default()
    };
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        config,
        Arc::new(Runtime::new().unwrap()),
        ExecutorRegistry::from_single_executor(executor, "default"),
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    app.update();
    assert_eq!(agent_descriptions(&mut app, "alpha:helper"), vec!["v1"]);

    // 修改 profile 后重载：插件 Agent 仍存在，且读取最新 profile。
    write_agent_plugin(plugins_dir.path(), "v2");
    reload_plugins(&mut app);
    assert_eq!(agent_descriptions(&mut app, "alpha:helper"), vec!["v2"]);
    assert_eq!(
        agent_descriptions(&mut app, "main"),
        vec!["主 Agent"],
        "配置文件中的 Agent 不受插件重载影响"
    );

    // 删除插件后重载：其贡献的 Agent 随之移除。
    std::fs::remove_dir_all(plugins_dir.path().join("alpha")).unwrap();
    reload_plugins(&mut app);
    assert!(agent_descriptions(&mut app, "alpha:helper").is_empty());
}
