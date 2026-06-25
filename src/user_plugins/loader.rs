use std::fs;
use std::path::{Path, PathBuf};

use rhai::{AST, Engine};
use tracing::{debug, warn};

use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::manifest::{ManifestError, PluginManifest};
use crate::user_plugins::registry::{LoadedPlugin, PluginLoadFailure, PluginRegistry};

/// 默认插件根目录
pub const DEFAULT_PLUGINS_DIR: &str = ".harness/plugins";

/// 扫描 `plugins_dir` 下每个子目录的 `manifest.toml`，加载校验通过的插件。
///
/// 失败的插件不会让整个加载过程 panic，只记入 registry.failures 并 warn 日志。
pub fn load_plugins_from_dir(plugins_dir: &Path) -> PluginRegistry {
    let mut registry = PluginRegistry::default();

    let entries = match fs::read_dir(plugins_dir) {
        Ok(entries) => entries,
        Err(err) => {
            debug!(
                event = "PluginsDirMissing",
                path = %plugins_dir.display(),
                error = %err,
                "plugins directory not present, loading no plugins"
            );
            return registry;
        }
    };

    // 先收集所有候选目录，再按 id 字母序处理（registry.insert 会再排序，但提前排序
    // 让日志顺序稳定）。
    let mut plugin_dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    plugin_dirs.sort();

    for plugin_dir in plugin_dirs {
        load_single_plugin(&plugin_dir, &mut registry);
    }

    if !registry.is_empty() {
        let loaded: Vec<&str> = registry
            .plugins()
            .iter()
            .map(|p| p.manifest.id.as_str())
            .collect();
        let failed: Vec<&str> = registry
            .failures()
            .iter()
            .map(|f| f.plugin_id.as_deref().unwrap_or("<unknown>"))
            .collect();
        debug!(
            event = "PluginsLoadedSummary",
            loaded = ?loaded,
            failed = ?failed,
            "plugin summary"
        );
    }

    registry
}

fn load_single_plugin(plugin_dir: &Path, registry: &mut PluginRegistry) {
    let manifest_path = plugin_dir.join("manifest.toml");
    let manifest_content = match fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(err) => {
            warn!(
                event = "PluginManifestMissing",
                path = %manifest_path.display(),
                error = %err,
                "skip plugin: manifest.toml not found"
            );
            registry.record_failure(PluginLoadFailure {
                plugin_id: None,
                root_dir: plugin_dir.to_path_buf(),
                error: format!("manifest.toml read failed: {err}"),
            });
            return;
        }
    };

    let manifest = match PluginManifest::from_toml(&manifest_content) {
        Ok(m) => m,
        Err(ManifestError::Parse(e)) => {
            warn!(
                event = "PluginManifestParseError",
                path = %manifest_path.display(),
                error = %e,
                "skip plugin: manifest parse"
            );
            registry.record_failure(PluginLoadFailure {
                plugin_id: None,
                root_dir: plugin_dir.to_path_buf(),
                error: format!("manifest parse: {e:?}"),
            });
            return;
        }
        err @ Err(ManifestError::Invalid(_)) => {
            let err = err.unwrap_err();
            if let ManifestError::Invalid(msg) = &err {
                warn!(
                    event = "PluginManifestInvalid",
                    path = %manifest_path.display(),
                    reason = %msg,
                    "skip plugin: manifest validation"
                );
            }
            registry.record_failure(PluginLoadFailure {
                plugin_id: None,
                root_dir: plugin_dir.to_path_buf(),
                error: format!("manifest invalid: {err:?}"),
            });
            return;
        }
    };

    let plugin_id = manifest.id.clone();

    match build_loaded_plugin(&manifest, plugin_dir) {
        Ok(loaded) => registry.insert(loaded),
        Err(err) => {
            warn!(
                event = "PluginAssetBuildFailed",
                plugin_id = %plugin_id,
                error = %err,
                "skip plugin: asset build"
            );
            registry.record_failure(PluginLoadFailure {
                plugin_id: Some(plugin_id),
                root_dir: plugin_dir.to_path_buf(),
                error: err,
            });
        }
    }
}

fn build_loaded_plugin(manifest: &PluginManifest, root_dir: &Path) -> Result<LoadedPlugin, String> {
    // 校验所有引用的文件存在 + 落在 root_dir 内（防穿越）。
    for path in manifest_files(manifest) {
        let abs = root_dir.join(&path);
        if !abs.exists() {
            return Err(format!(
                "missing file referenced by manifest: {}",
                path.display()
            ));
        }
        if !is_within(root_dir, &abs) {
            return Err(format!(
                "manifest references file outside plugin root: {}",
                path.display()
            ));
        }
    }

    // 静态编译 Rhai 脚本
    let engine = new_sandboxed_engine();
    let mut hook_asts: std::collections::HashMap<HookPoint, Vec<AST>> =
        std::collections::HashMap::new();
    for hook in &manifest.hooks {
        let point: HookPoint = hook.event.parse().map_err(|e| format!("{}", e))?;
        let script_path = root_dir.join(&hook.script);
        let source = fs::read_to_string(&script_path)
            .map_err(|e| format!("read hook script {}: {e}", script_path.display()))?;
        let ast = engine
            .compile(&source)
            .map_err(|e| format!("compile {}: {e}", script_path.display()))?;
        hook_asts.entry(point).or_default().push(ast);
    }

    let mut command_asts = std::collections::HashMap::new();
    for cmd in &manifest.commands {
        let script_path = root_dir.join(&cmd.script);
        let source = fs::read_to_string(&script_path)
            .map_err(|e| format!("read command script {}: {e}", script_path.display()))?;
        let ast = engine
            .compile(&source)
            .map_err(|e| format!("compile {}: {e}", script_path.display()))?;
        command_asts.insert(cmd.id.clone(), ast);
    }

    let mut tool_asts = std::collections::HashMap::new();
    for tool in &manifest.tools {
        // 读取并校验 JSON Schema
        let schema_path = root_dir.join(&tool.schema);
        let schema_str = fs::read_to_string(&schema_path)
            .map_err(|e| format!("read tool schema {}: {e}", schema_path.display()))?;
        let schema_value: serde_json::Value = serde_json::from_str(&schema_str)
            .map_err(|e| format!("parse tool schema {}: {e}", schema_path.display()))?;
        if let Err(e) = jsonschema::validator_for(&schema_value) {
            warn!(
                event = "PluginToolSchemaInvalid",
                plugin_id = %manifest.id,
                tool_id = %tool.id,
                error = %e,
                "tool schema is not a valid JSON Schema, skipping this tool"
            );
            continue;
        }

        let script_path = root_dir.join(&tool.handler);
        let source = fs::read_to_string(&script_path)
            .map_err(|e| format!("read tool handler {}: {e}", script_path.display()))?;
        let ast = engine
            .compile(&source)
            .map_err(|e| format!("compile {}: {e}", script_path.display()))?;
        tool_asts.insert(tool.id.clone(), ast);
    }

    Ok(LoadedPlugin {
        manifest: manifest.clone(),
        root_dir: root_dir.to_path_buf(),
        hook_asts,
        command_asts,
        tool_asts,
        state: std::collections::HashMap::new(),
        temp_resources: std::collections::HashMap::new(),
    })
}

fn manifest_files(manifest: &PluginManifest) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for h in &manifest.hooks {
        out.push(h.script.clone());
    }
    for t in &manifest.tools {
        out.push(t.schema.clone());
        out.push(t.handler.clone());
    }
    for s in &manifest.skills {
        out.push(s.path.clone());
    }
    for a in &manifest.agents {
        out.push(a.profile.clone());
    }
    for c in &manifest.commands {
        out.push(c.script.clone());
    }
    out
}

/// canonicalize 后做前缀检查，确认 abs 在 root 之内（含 root 本身）。
pub fn is_within(root: &Path, abs: &Path) -> bool {
    let root_c = std::fs::canonicalize(root).ok();
    let abs_c = std::fs::canonicalize(abs).ok();
    match (root_c, abs_c) {
        (Some(r), Some(a)) => a.starts_with(&r),
        _ => abs.starts_with(root),
    }
}

/// 创建禁用 FS / 网络的 Rhai Engine。
///
/// 仅靠不注册任何 std 原语来沙箱化。我们要的 Host API 全部通过
/// `register_fn` 显式绑定。
pub fn new_sandboxed_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_call_levels(32);
    engine
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_plugin(root: &Path, manifest: &str, files: &[(&str, &str)]) {
        fs::write(root.join("manifest.toml"), manifest).unwrap();
        for (path, content) in files {
            let p = root.join(path);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(p, content).unwrap();
        }
    }

    #[test]
    fn loads_valid_plugin_with_hook() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("alpha");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(
            &plugin_dir,
            r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/on_task_created.rhai"
"#,
            &[("hooks/on_task_created.rhai", "log_info(\"hello\");\n")],
        );

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 1);
        assert_eq!(registry.plugins()[0].manifest.id, "alpha");
        assert!(
            registry.plugins()[0]
                .hook_asts
                .contains_key(&HookPoint::OnTaskCreated)
        );
    }

    #[test]
    fn missing_manifest_records_failure_and_continues() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("no-manifest")).unwrap();
        let good = dir.path().join("good");
        fs::create_dir(&good).unwrap();
        write_plugin(&good, "id = \"good\"\napi_version = 1\n", &[]);

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 1);
        assert_eq!(registry.failures().len(), 1);
    }

    #[test]
    fn wrong_api_version_skips_plugin() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(&plugin_dir, "id = \"bad\"\napi_version = 99\n", &[]);

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 0);
        assert_eq!(registry.failures().len(), 1);
    }

    #[test]
    fn compile_error_skips_plugin() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad-syntax");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(
            &plugin_dir,
            r#"
id = "bad-syntax"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/x.rhai"
"#,
            &[("hooks/x.rhai", "let x = ;\n")],
        );

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 0);
        assert_eq!(registry.failures().len(), 1);
    }

    #[test]
    fn invalid_tool_schema_skips_tool_but_loads_plugin() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad-schema");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(
            &plugin_dir,
            r#"
id = "bad-schema"
api_version = 1
[[tools]]
id = "broken"
description = "a tool with bad schema"
schema = "tools/broken.schema.json"
handler = "tools/broken.rhai"
"#,
            &[
                // Schema 声明 type="object" 但 properties 不是 object，属于无效 JSON Schema
                (
                    "tools/broken.schema.json",
                    r#"{"type": "object", "properties": 42}"#,
                ),
                ("tools/broken.rhai", "42\n"),
            ],
        );

        let registry = load_plugins_from_dir(dir.path());
        // 插件本身可以加载，但 tool_asts 不包含 schema 无效的 tool
        assert_eq!(registry.plugins().len(), 1);
        assert_eq!(registry.plugins()[0].manifest.id, "bad-schema");
        assert!(
            registry.plugins()[0].tool_asts.is_empty(),
            "tool with invalid schema should be skipped"
        );
    }

    #[test]
    fn valid_tool_schema_loads_successfully() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("good-tool");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(
            &plugin_dir,
            r#"
id = "good-tool"
api_version = 1
[[tools]]
id = "search"
description = "search tool"
schema = "tools/search.schema.json"
handler = "tools/search.rhai"
"#,
            &[
                (
                    "tools/search.schema.json",
                    r#"{"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}"#,
                ),
                ("tools/search.rhai", "42\n"),
            ],
        );

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 1);
        assert_eq!(registry.plugins()[0].tool_asts.len(), 1);
        assert!(registry.plugins()[0].tool_asts.contains_key("search"));
    }
}
