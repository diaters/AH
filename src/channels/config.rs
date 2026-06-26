use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfigs {
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
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
}
