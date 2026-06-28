use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfigs {
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub qq: Option<QqConfig>,
}

impl ChannelConfigs {
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
