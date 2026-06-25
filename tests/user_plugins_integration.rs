//! Task 45 集成测试：插件加载路径。
//!
//! 验证：
//! - fixture test-plugin 能成功加载
//! - api_version 不匹配的插件进入 failures
//! - 坏插件不阻塞好插件加载
//! - fixture tool 具有有效 schema

use std::path::PathBuf;

use harness::user_plugins::loader::load_plugins_from_dir;

/// fixture 插件目录
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("plugins")
}

#[test]
fn fixture_plugin_loads() {
    let registry = load_plugins_from_dir(&fixtures_dir());
    assert!(
        registry
            .plugins()
            .iter()
            .any(|p| p.manifest.id == "test-plugin"),
        "test-plugin 应在成功加载列表中"
    );
    assert!(
        registry.failures().is_empty(),
        "fixture 插件不应产生任何加载失败，但 failures = {:?}",
        registry.failures()
    );
}

#[test]
fn bad_api_version_plugin_goes_to_failures() {
    let dir = tempfile::TempDir::new().unwrap();
    let plugin_dir = dir.path().join("bad-api");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "bad-api"
api_version = 999
"#,
    )
    .unwrap();

    let registry = load_plugins_from_dir(dir.path());
    assert!(
        registry.plugins().is_empty(),
        "api_version=999 的插件不应成功加载"
    );
    assert_eq!(registry.failures().len(), 1, "应有恰好一条加载失败记录");
}

#[test]
fn bad_plugin_does_not_block_good_plugin() {
    let dir = tempfile::TempDir::new().unwrap();

    // 好插件
    let good_dir = dir.path().join("good-one");
    std::fs::create_dir_all(&good_dir).unwrap();
    std::fs::write(
        good_dir.join("manifest.toml"),
        r#"
id = "good-one"
api_version = 1
"#,
    )
    .unwrap();

    // 坏插件（api_version 不匹配）
    let bad_dir = dir.path().join("bad-one");
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(
        bad_dir.join("manifest.toml"),
        r#"
id = "bad-one"
api_version = 999
"#,
    )
    .unwrap();

    let registry = load_plugins_from_dir(dir.path());
    assert_eq!(registry.plugins().len(), 1, "好插件应成功加载");
    assert_eq!(
        registry.plugins()[0].manifest.id,
        "good-one",
        "成功加载的应为 good-one"
    );
    assert_eq!(registry.failures().len(), 1, "坏插件应产生一条失败记录");
}

#[test]
fn fixture_tool_has_valid_schema() {
    let registry = load_plugins_from_dir(&fixtures_dir());
    let plugin = registry
        .plugins()
        .iter()
        .find(|p| p.manifest.id == "test-plugin")
        .expect("test-plugin 应已加载");

    // 验证 tool_asts 中包含 "hello"（schema 合法则 tool 会被编译注册）
    assert!(
        plugin.tool_asts.contains_key("hello"),
        "test-plugin 的 hello tool 应有预编译 AST（schema 合法）"
    );

    // 验证 manifest 中工具声明存在
    let tool_def = plugin
        .manifest
        .tools
        .iter()
        .find(|t| t.id == "hello")
        .expect("manifest 中应声明 hello tool");
    assert_eq!(
        tool_def.description, "Return a friendly greeting",
        "hello tool 描述应与 manifest 一致"
    );
}
