use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfigs {
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub qq: Option<QqConfig>,
}

impl ChannelConfigs {
    /// 将 `user` 追加到 `[qq].allowed_users` 并持久化到 `path`；文件缺失 `[qq]` 段时以 `default_qq` 初始化。
    pub async fn append_qq_allowed_user(
        path: &std::path::Path,
        user: &str,
        default_qq: QqConfig,
    ) -> Result<(), String> {
        Self::update_file(path, |configs| {
            let qq = configs.qq.get_or_insert(default_qq);
            if !qq.allowed_users.iter().any(|u| u == user) {
                qq.allowed_users.push(user.to_string());
            }
        })
        .await
    }

    /// 将 `user` 追加到 `[telegram].allowed_users` 并持久化到 `path`；
    /// 文件缺失 `[telegram]` 段时以 `default_telegram` 初始化。
    pub async fn append_telegram_allowed_user(
        path: &std::path::Path,
        user: &str,
        default_telegram: TelegramConfig,
    ) -> Result<(), String> {
        Self::update_file(path, |configs| {
            let tg = configs.telegram.get_or_insert(default_telegram);
            if !tg.allowed_users.iter().any(|u| u == user) {
                tg.allowed_users.push(user.to_string());
            }
        })
        .await
    }

    /// 读取 → 修改 → 原子写回配置文件。
    ///
    /// 配置文件 IO 与格式知识收口于此（通道模块不直接写配置）：以 `ChannelConfigs`
    /// 全量读写，确保其他段不丢失。文件不存在时从空配置开始；文件存在但读取或解析
    /// 失败时返回错误、不覆盖原文件，避免丢掉其他通道的配置与凭据。
    async fn update_file(
        path: &std::path::Path,
        apply: impl FnOnce(&mut Self),
    ) -> Result<(), String> {
        let mut configs: Self = match tokio::fs::read_to_string(path).await {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| format!("refuse to overwrite unparseable {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        apply(&mut configs);
        let content = toml::to_string_pretty(&configs).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("toml.tmp");
        tokio::fs::write(&tmp, content)
            .await
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        tokio::fs::rename(&tmp, path)
            .await
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))
    }

    /// 展开配置文件中的环境变量引用。
    ///
    /// 当前处理 Telegram 和 QQ 的凭证字段：
    /// - 支持 `${VAR}` 形式展开；
    /// - 若展开后为空，则尝试回退读取对应环境变量。
    pub fn expand_env_vars(&mut self) {
        if let Some(tg) = &mut self.telegram {
            tg.bot_token = expand_env_var(&tg.bot_token);
            if tg.bot_token.is_empty()
                && let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN")
            {
                tg.bot_token = token;
            }
        }
        if let Some(qq) = &mut self.qq {
            qq.app_id = expand_env_var(&qq.app_id);
            if qq.app_id.is_empty()
                && let Ok(v) = std::env::var("QQ_APP_ID")
            {
                qq.app_id = v;
            }
            qq.app_secret = expand_env_var(&qq.app_secret);
            if qq.app_secret.is_empty()
                && let Ok(v) = std::env::var("QQ_APP_SECRET")
            {
                qq.app_secret = v;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// 可代任意群成员审批的用户 ID；为空时群聊审批只接受任务发起人
    #[serde(default)]
    pub approvers: Vec<String>,
    #[serde(default)]
    pub pairing_enabled: bool,
    pub pairing_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QqConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// 可代任意群成员审批的用户 ID；为空时群聊审批只接受任务发起人
    #[serde(default)]
    pub approvers: Vec<String>,
    #[serde(default)]
    pub pairing_enabled: bool,
    pub pairing_code: Option<String>,
}

/// 展开字符串中的 `${VAR}` 环境变量引用。
fn expand_env_var(value: &str) -> String {
    let mut result = value.to_string();
    while let Some(start) = result.find("${") {
        let rest = &result[start + 2..];
        if let Some(end) = rest.find('}') {
            let var_name = &rest[..end];
            let replacement = std::env::var(var_name).unwrap_or_default();
            result.replace_range(start..start + 2 + end + 1, &replacement);
        } else {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qq_default() -> QqConfig {
        QqConfig {
            approvers: Vec::new(),
            app_id: "id".to_string(),
            app_secret: "secret".to_string(),
            allowed_users: vec![],
            pairing_enabled: true,
            pairing_code: Some("code".to_string()),
        }
    }

    /// 配置文件无法解析时不能用默认值整体覆盖，否则会丢掉其他通道的配置与凭据。
    #[tokio::test]
    async fn append_allowed_user_refuses_to_overwrite_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channels.toml");
        let original = "[telegram]\nbot_token = \"keep-me\"\nallowed_users = [\n";
        std::fs::write(&path, original).unwrap();

        let result = ChannelConfigs::append_qq_allowed_user(&path, "u1", qq_default()).await;

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn append_allowed_user_preserves_other_sections_and_writes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channels.toml");
        std::fs::write(&path, "[telegram]\nbot_token = \"keep-me\"\n").unwrap();

        ChannelConfigs::append_qq_allowed_user(&path, "u1", qq_default())
            .await
            .unwrap();
        let tg_default = TelegramConfig {
            approvers: Vec::new(),
            bot_token: "from-memory".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        };
        ChannelConfigs::append_telegram_allowed_user(&path, "t1", tg_default)
            .await
            .unwrap();

        let cfg: ChannelConfigs = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let tg = cfg.telegram.unwrap();
        assert_eq!(tg.bot_token, "keep-me");
        assert_eq!(tg.allowed_users, vec!["t1".to_string()]);
        assert_eq!(cfg.qq.unwrap().allowed_users, vec!["u1".to_string()]);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "temporary file must be renamed away");
    }

    #[test]
    fn parse_telegram_config() {
        let toml = r#"
[telegram]
bot_token = "xxx"
allowed_users = ["alice"]
"#;
        let cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
        let tg = cfg.telegram.expect("telegram present");
        assert_eq!(tg.bot_token, "xxx");
        assert_eq!(tg.allowed_users, vec!["alice".to_string()]);
    }

    #[test]
    fn empty_config_is_default() {
        let cfg: ChannelConfigs = toml::from_str("").expect("parse empty");
        assert!(cfg.telegram.is_none());
        assert!(cfg.qq.is_none());
    }

    #[test]
    fn expand_env_var_in_bot_token() {
        let toml = r#"
[telegram]
bot_token = "${TEST_TELEGRAM_BOT_TOKEN}"
allowed_users = ["alice"]
"#;
        let mut cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
        unsafe {
            std::env::set_var("TEST_TELEGRAM_BOT_TOKEN", "secret-token");
        }
        cfg.expand_env_vars();
        assert_eq!(cfg.telegram.unwrap().bot_token, "secret-token");
        unsafe {
            std::env::remove_var("TEST_TELEGRAM_BOT_TOKEN");
        }
    }

    #[test]
    fn fallback_to_telegram_bot_token_env() {
        let toml = r#"
[telegram]
bot_token = ""
allowed_users = ["alice"]
"#;
        let mut cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
        unsafe {
            std::env::set_var("TELEGRAM_BOT_TOKEN", "env-token");
        }
        cfg.expand_env_vars();
        assert_eq!(cfg.telegram.unwrap().bot_token, "env-token");
        unsafe {
            std::env::remove_var("TELEGRAM_BOT_TOKEN");
        }
    }

    #[test]
    fn parse_qq_config() {
        let toml = r#"
[qq]
app_id = "12345"
app_secret = "secret_abc"
allowed_users = ["user1"]
"#;
        let cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
        let qq = cfg.qq.expect("qq present");
        assert_eq!(qq.app_id, "12345");
        assert_eq!(qq.app_secret, "secret_abc");
        assert_eq!(qq.allowed_users, vec!["user1".to_string()]);
    }

    #[test]
    fn qq_config_defaults_pairing_disabled() {
        let toml = r#"
[qq]
app_id = "x"
app_secret = "y"
"#;
        let cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
        let qq = cfg.qq.expect("qq present");
        assert!(!qq.pairing_enabled);
        assert!(qq.pairing_code.is_none());
    }

    #[test]
    fn expand_env_var_in_qq_app_id() {
        let toml = r#"
[qq]
app_id = "${TEST_QQ_APP_ID}"
app_secret = "secret"
"#;
        let mut cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
        unsafe {
            std::env::set_var("TEST_QQ_APP_ID", "expanded-id");
        }
        cfg.expand_env_vars();
        assert_eq!(cfg.qq.unwrap().app_id, "expanded-id");
        unsafe {
            std::env::remove_var("TEST_QQ_APP_ID");
        }
    }

    #[test]
    fn fallback_to_qq_app_secret_env() {
        let toml = r#"
[qq]
app_id = "id"
app_secret = ""
"#;
        let mut cfg: ChannelConfigs = toml::from_str(toml).expect("parse");
        unsafe {
            std::env::set_var("QQ_APP_SECRET", "env-secret");
        }
        cfg.expand_env_vars();
        assert_eq!(cfg.qq.unwrap().app_secret, "env-secret");
        unsafe {
            std::env::remove_var("QQ_APP_SECRET");
        }
    }
}
