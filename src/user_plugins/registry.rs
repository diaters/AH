use std::collections::HashMap;
use std::path::PathBuf;

use crate::prelude::Resource;
use rhai::AST;

use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::manifest::PluginManifest;

/// 一个已通过校验、加载到内存的插件。
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root_dir: PathBuf,
    /// 预编译的 hook 脚本，按 hook 点分组。
    pub hook_asts: HashMap<HookPoint, Vec<AST>>,
    /// 预编译的 slash command 脚本。
    pub command_asts: HashMap<String, AST>,
    /// 预编译的 tool handler 脚本。
    pub tool_asts: HashMap<String, AST>,
    /// 该插件的 per-plugin state。
    pub state: HashMap<String, rhai::Dynamic>,
    /// 该插件贡献的临时资源，reload 时清空。
    pub temp_resources: HashMap<String, rhai::Dynamic>,
}

impl LoadedPlugin {
    /// 全局命名空间的 tool id。
    pub fn namespaced_tool_id(&self, local_id: &str) -> String {
        format!("{}:{}", self.manifest.id, local_id)
    }

    pub fn namespaced_agent_id(&self, local_id: &str) -> String {
        format!("{}:{}", self.manifest.id, local_id)
    }

    pub fn namespaced_skill_id(&self, local_id: &str) -> String {
        format!("{}:{}", self.manifest.id, local_id)
    }
}

/// 全局插件注册表。
///
/// 按 manifest.id 字母序保存通过校验的插件。其它系统通过此 Resource
/// 查询当前可用的插件贡献。
#[derive(Resource, Debug, Default)]
pub struct PluginRegistry {
    plugins: Vec<LoadedPlugin>,
    failed: Vec<PluginLoadFailure>,
}

#[derive(Debug, Clone)]
pub struct PluginLoadFailure {
    pub plugin_id: Option<String>,
    pub root_dir: PathBuf,
    pub error: String,
}

impl PluginRegistry {
    /// 按 manifest.id 字母序插入。重复 id 视为第二次冲突，插入失败列表。
    /// 若该插件任意 `command.display` 与已注册插件冲突，也插入失败列表，
    /// 不影响已经注册的插件（后注册者被跳过）。
    pub fn insert(&mut self, plugin: LoadedPlugin) {
        let id = plugin.manifest.id.clone();
        if self.plugins.iter().any(|p| p.manifest.id == id) {
            self.failed.push(PluginLoadFailure {
                plugin_id: Some(id),
                root_dir: plugin.root_dir.clone(),
                error: "duplicate plugin id".into(),
            });
            return;
        }
        // 检查跨插件 command.display 冲突
        let conflicts: Vec<String> = plugin
            .manifest
            .commands
            .iter()
            .filter(|c| {
                self.plugins
                    .iter()
                    .any(|p| p.manifest.commands.iter().any(|oc| oc.display == c.display))
            })
            .map(|c| c.display.clone())
            .collect();
        if !conflicts.is_empty() {
            self.failed.push(PluginLoadFailure {
                plugin_id: Some(id),
                root_dir: plugin.root_dir.clone(),
                error: format!(
                    "command.display conflicts with already-loaded plugin(s): {}",
                    conflicts.join(", ")
                ),
            });
            return;
        }
        let pos = self
            .plugins
            .partition_point(|p| p.manifest.id < plugin.manifest.id);
        self.plugins.insert(pos, plugin);
    }

    pub fn record_failure(&mut self, failure: PluginLoadFailure) {
        self.failed.push(failure);
    }

    /// 所有成功加载的插件。
    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// 失败清单。
    pub fn failures(&self) -> &[PluginLoadFailure] {
        &self.failed
    }

    /// 查找拥有该 id 的插件。
    pub fn get(&self, id: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut LoadedPlugin> {
        self.plugins.iter_mut().find(|p| p.manifest.id == id)
    }

    /// 返回订阅指定 hook 点的所有插件（按已排序的字母序）。
    pub fn subscribers_for(&self, point: HookPoint) -> Vec<&LoadedPlugin> {
        self.plugins
            .iter()
            .filter(|p| p.manifest.subscribes_to(point.as_serialized()))
            .collect()
    }

    /// 清空所有数据，用于 /reload-plugins。
    pub fn clear(&mut self) {
        self.plugins.clear();
        self.failed.clear();
    }

    /// 是否没有任何插件加载（含失败也算非空）。
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty() && self.failed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_plugins::manifest::PluginManifest;

    fn make_plugin(id: &str) -> LoadedPlugin {
        LoadedPlugin {
            manifest: PluginManifest {
                id: id.to_string(),
                name: None,
                version: None,
                api_version: 1,
                author: None,
                description: None,
                hooks: vec![],
                tools: vec![],
                skills: vec![],
                agents: vec![],
                commands: vec![],
            },
            root_dir: PathBuf::from("/tmp"),
            hook_asts: HashMap::new(),
            command_asts: HashMap::new(),
            tool_asts: HashMap::new(),
            state: HashMap::new(),
            temp_resources: HashMap::new(),
        }
    }

    #[test]
    fn inserts_sorted_by_id() {
        let mut reg = PluginRegistry::default();
        reg.insert(make_plugin("zebra"));
        reg.insert(make_plugin("alpha"));
        reg.insert(make_plugin("middle"));

        let ids: Vec<_> = reg
            .plugins()
            .iter()
            .map(|p| p.manifest.id.as_str())
            .collect();
        assert_eq!(ids, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn duplicate_id_goes_to_failures() {
        let mut reg = PluginRegistry::default();
        reg.insert(make_plugin("dup"));
        reg.insert(make_plugin("dup"));

        assert_eq!(reg.plugins().len(), 1);
        assert_eq!(reg.failures().len(), 1);
    }

    #[test]
    fn duplicate_display_across_plugins_goes_to_failures() {
        let mut reg = PluginRegistry::default();

        let mut first = make_plugin("alpha");
        first
            .manifest
            .commands
            .push(crate::user_plugins::manifest::CommandContribution {
                id: "hi".to_string(),
                display: "/hi".to_string(),
                script: PathBuf::from("commands/hi.rhai"),
                description: None,
            });
        reg.insert(first);

        let mut second = make_plugin("beta");
        second
            .manifest
            .commands
            .push(crate::user_plugins::manifest::CommandContribution {
                id: "hi".to_string(),
                display: "/hi".to_string(),
                script: PathBuf::from("commands/hi.rhai"),
                description: None,
            });
        reg.insert(second);

        // 先注册者保留
        assert_eq!(reg.plugins().len(), 1);
        assert_eq!(reg.plugins()[0].manifest.id, "alpha");
        // 后注册者跳到 failures
        assert_eq!(reg.failures().len(), 1);
        assert!(reg.failures()[0].error.contains("display"));
    }
}
