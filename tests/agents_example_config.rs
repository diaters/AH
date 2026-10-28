//! `agents.toml.example` 守护测试：示例必须可解析，且引用的工具名都真实存在，
//! 避免示例随工具面演进而腐化（曾残留 spawn_agent / read_file 等已移除的工具）。

use harness::domain::{AgentConfig, BuiltinToolExecutors, SpaceToolRegistry};
use harness::systems::tools::register_builtin_tools;

#[test]
fn agents_example_parses_and_references_only_existing_tools() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/agents.toml.example"))
        .expect("agents.toml.example should exist");
    let config: AgentConfig = toml::from_str(&text).expect("agents.toml.example should parse");

    let mut registry = SpaceToolRegistry::default();
    let mut executors = BuiltinToolExecutors::default();
    register_builtin_tools(&mut registry, &mut executors);

    for agent in &config.agent {
        let Some(tools) = &agent.tools else { continue };
        for tool in tools.overrides.keys() {
            assert!(
                registry.get(tool).is_some(),
                "agent '{}' references unknown tool '{tool}' in agents.toml.example",
                agent.name
            );
        }
    }
}
