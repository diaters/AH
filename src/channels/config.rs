use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfigs {
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
}

impl ChannelConfigs {
    /// 展开配置文件中的环境变量引用。
    ///
    /// 当前仅处理 Telegram 的 `bot_token`：
    /// - 支持 `${VAR}` 形式展开；
    /// - 若展开后为空，则尝试回退读取 `TELEGRAM_BOT_TOKEN` 环境变量。
    pub fn expand_env_vars(&mut self) {
        if let Some(tg) = &mut self.telegram {
            tg.bot_token = expand_env_var(&tg.bot_token);
            if tg.bot_token.is_empty()
                && let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN")
            {
                tg.bot_token = token;
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
}
