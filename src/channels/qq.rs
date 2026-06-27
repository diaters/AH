//! QQ 官方 Bot API 通道实现

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tokio::sync::RwLock;

use crate::channels::config::QqConfig;
use crate::channels::traits::{Channel, ChannelError};

const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_AUTH_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

/// QQ 通道实现
#[allow(dead_code)]
pub struct QqChannel {
    config: QqConfig,
    config_path: Option<PathBuf>,
    runtime_allowed_users: Arc<RwLock<HashSet<String>>>,
    client: Client,
    token_cache: Arc<RwLock<Option<(String, u64)>>>,
    workspace_dir: Option<PathBuf>,
    api_base: String,
    auth_url: String,
}

impl QqChannel {
    pub fn new(config: QqConfig) -> Self {
        Self::new_with_path(config, None)
    }

    pub fn new_with_path(config: QqConfig, config_path: Option<PathBuf>) -> Self {
        Self {
            config,
            config_path,
            runtime_allowed_users: Arc::new(RwLock::new(HashSet::new())),
            client: Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
            workspace_dir: None,
            api_base: QQ_API_BASE.to_string(),
            auth_url: QQ_AUTH_URL.to_string(),
        }
    }

    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// 测试用：覆盖 API base URL。
    #[cfg(test)]
    #[allow(dead_code)]
    fn with_api_base(mut self, base: String) -> Self {
        self.api_base = base;
        self
    }

    /// 测试用：覆盖 OAuth2 端点 URL。
    #[cfg(test)]
    fn with_auth_url(mut self, url: String) -> Self {
        self.auth_url = url;
        self
    }

    /// 测试用：预置 token 避免真实 OAuth2 调用。
    #[cfg(test)]
    #[allow(dead_code)]
    async fn set_token_for_test(&self, token: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut cache = self.token_cache.write().await;
        *cache = Some((token.to_string(), now + 3600));
    }

    /// 向 QQ OAuth2 端点获取 access_token，返回 (token, 过期时间戳)。
    async fn fetch_access_token(&self) -> Result<(String, u64), ChannelError> {
        let body = json!({
            "appId": self.config.app_id,
            "clientSecret": self.config.app_secret,
        });
        let resp = self.client.post(&self.auth_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        let data: serde_json::Value = resp.json().await?;
        let token = data
            .get("access_token")
            .and_then(|t| t.as_str())
            .ok_or(ChannelError::Auth)?
            .to_string();
        let expires_in = data
            .get("expires_in")
            .and_then(|e| e.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(7200);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expiry = now + expires_in.saturating_sub(60);
        Ok((token, expiry))
    }

    /// 获取有效 access_token，过期时重新获取。
    #[allow(dead_code)]
    async fn get_token(&self) -> Result<String, ChannelError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        {
            let cache = self.token_cache.read().await;
            if let Some((ref token, expiry)) = *cache
                && now < expiry
            {
                return Ok(token.clone());
            }
        }
        let (token, expiry) = self.fetch_access_token().await?;
        {
            let mut cache = self.token_cache.write().await;
            *cache = Some((token.clone(), expiry));
        }
        Ok(token)
    }
}

#[async_trait]
impl Channel for QqChannel {
    fn name(&self) -> &str {
        "qq"
    }

    async fn send(
        &self,
        _message: &crate::channels::traits::ChannelOutboundMessage,
    ) -> Result<(), ChannelError> {
        // 占位实现，后续 Task 填充
        Ok(())
    }

    async fn listen(
        &self,
        _tx: crossbeam_channel::Sender<crate::channels::traits::ChannelInboundMessage>,
    ) -> Result<(), ChannelError> {
        // 占位实现，后续 Task 填充
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.fetch_access_token().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> QqConfig {
        QqConfig {
            app_id: "test_id".to_string(),
            app_secret: "test_secret".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        }
    }

    #[test]
    fn name_returns_qq() {
        let ch = QqChannel::new(make_config());
        assert_eq!(ch.name(), "qq");
    }

    #[test]
    fn workspace_dir_builder_sets_field() {
        let ch = QqChannel::new(make_config()).with_workspace_dir(PathBuf::from("/tmp/qq"));
        assert_eq!(ch.workspace_dir, Some(PathBuf::from("/tmp/qq")));
    }

    #[tokio::test]
    async fn fetch_access_token_parses_response() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/getAppAccessToken"))
            .and(body_json(serde_json::json!({
                "appId": "test_id",
                "clientSecret": "test_secret",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "mock_token_abc",
                "expires_in": "7200",
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config())
            .with_auth_url(format!("{}/app/getAppAccessToken", mock_server.uri()));
        let (token, expiry) = ch.fetch_access_token().await.expect("fetch_access_token");
        assert_eq!(token, "mock_token_abc");
        // expiry 应在未来 7200-60=7140 秒处
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        assert!(expiry > now + 7000 && expiry < now + 7200);
    }

    #[tokio::test]
    async fn fetch_access_token_returns_auth_error_on_missing_field() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/getAppAccessToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "some_other_field": "no_token"
            })))
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config())
            .with_auth_url(format!("{}/app/getAppAccessToken", mock_server.uri()));
        let result = ch.fetch_access_token().await;
        assert!(matches!(result, Err(ChannelError::Auth)));
    }

    #[tokio::test]
    async fn token_cache_reuse_within_expiry() {
        let ch = QqChannel::new(make_config());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // 手动写入未过期 token
        {
            let mut cache = ch.token_cache.write().await;
            *cache = Some(("cached_token".to_string(), now + 3600));
        }
        let token = ch.get_token().await.expect("get_token");
        assert_eq!(token, "cached_token");
    }

    #[tokio::test]
    async fn token_cache_expired_triggers_refetch() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/getAppAccessToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh_token",
                "expires_in": "3600",
            })))
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config())
            .with_auth_url(format!("{}/app/getAppAccessToken", mock_server.uri()));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // 写入已过期 token
        {
            let mut cache = ch.token_cache.write().await;
            *cache = Some(("old_token".to_string(), now - 1));
        }
        let token = ch.get_token().await.expect("get_token");
        assert_eq!(token, "fresh_token");
    }
}
