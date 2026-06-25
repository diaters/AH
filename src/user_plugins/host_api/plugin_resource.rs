use std::path::PathBuf;
use std::sync::Arc;

use rhai::Engine;
use tracing::warn;

use crate::user_plugins::loader::is_within;

#[derive(Clone)]
pub struct PluginRoots {
    pub roots: Arc<Vec<PathBuf>>,
}

impl PluginRoots {
    pub fn single(root: PathBuf) -> Self {
        Self {
            roots: Arc::new(vec![root]),
        }
    }
}

pub fn register(engine: &mut Engine, roots: PluginRoots) {
    let r = roots.clone();
    engine.register_fn("read_plugin_resource", move |rel: &str| -> String {
        // 只能为该插件自己的根目录。dispatcher 注入的 PluginRoots 已仅含该插件根。
        let root = match r.roots.first() {
            Some(p) => p,
            None => return String::new(),
        };
        let candidate = root.join(rel);
        if !is_within(root, &candidate) {
            warn!(
                event = "PluginResourceAccessDenied",
                path = %candidate.display(),
                "plugin tried to read outside its root"
            );
            return String::new();
        }
        match std::fs::read_to_string(&candidate) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    event = "PluginResourceReadError",
                    path = %candidate.display(),
                    error = %e,
                    "failed to read plugin resource"
                );
                String::new()
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn reads_file_within_root() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let mut e = Engine::new();
        register(&mut e, PluginRoots::single(dir.path().to_path_buf()));
        let content: String = e.eval(r#"read_plugin_resource("hello.txt")"#).unwrap();
        assert_eq!(content, "hi");
    }

    #[test]
    fn traversal_outside_root_returns_empty() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret.txt"), "ssh").unwrap();

        let mut e = Engine::new();
        register(&mut e, PluginRoots::single(dir.path().to_path_buf()));
        let rel = format!(
            "../{}",
            outside.path().file_name().unwrap().to_string_lossy()
        );
        let content: String = e
            .eval(&format!(r#"read_plugin_resource("{rel}/secret.txt")"#))
            .unwrap();
        // canonicalize 检查后应拒绝跨目录读取
        assert_eq!(content, "");
    }
}
