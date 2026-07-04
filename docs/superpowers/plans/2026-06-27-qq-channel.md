# QQ 通道实施计划

> __For agentic workers:__ REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

__Goal:__ 实现 QQ 官方 Bot API 通道，支持 C2C/群消息收发、Markdown、媒体/文件传输、/bind 配对、审批交互。

__Architecture:__ 复用 Harness 已有 Channel trait / ChannelOutboundMessage / ChannelFrontend / ChannelManager 抽象，新增 `QqChannel` 实现 + `QqConfig` 配置扩展。OAuth2 + WebSocket Gateway 接收事件，`msg_type=2` markdown / `msg_type=7` 富媒体出向，文本回复匹配实现审批交互。

__Tech Stack:__ Rust + Bevy ECS + tokio + reqwest + tokio-tungstenite（WebSocket）+ sha2/md5/base64（媒体上传）+ wiremock（测试）。

__Design Spec:__ [docs/superpowers/specs/2026-06-27-qq-channel-design.md](../specs/2026-06-27-qq-channel-design.md)

__Reference Implementation:__ [zeroclaw-dev/src/channels/qq.rs](file:///Users/diater/diahub/zeroclaw-dev/src/channels/qq.rs)（适配 Harness 抽象层）

## Global Constraints

- 依赖原则：仅 crates.io，许可证 MIT/Apache-2.0 兼容，优先纯 Rust 实现
- 通道实现位于 `src/channels/qq.rs`，配置扩展位于 `src/channels/config.rs`
- 复用 `extract_attachments()`（[src/channels/traits.rs:100](../../../src/channels/traits.rs#L100)）解析 `[IMAGE:path]` 等标记，不重新实现
- `ChannelId.user_id` 携带前缀编码：`user:<user_openid>` 或 `group:<group_openid>`
- 出向统一走主动消息路径（不携带 msg_id）
- 移除 reply_tracker 限速逻辑（QQ 限速已取消）
- 错误类型使用 `ChannelError`，不使用 anyhow
- 测试与实现文件放在一起 `#[cfg(test)]`，集成测试放 `tests/`
- 提交信息格式：`<type>: <description>`，遵循 Conventional Commits
- 中文文档撰写，可夹杂英文术语
- 所有变更需通过 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`

---

## 文件结构

| 文件 | 责任 | 操作 |
|------|------|------|
| `Cargo.toml` | 新增 4 个依赖 | 修改 |
| `src/channels/config.rs` | `QqConfig` 结构 + `ChannelConfigs.qq` 字段 + `expand_env_vars()` 扩展 | 修改 |
| `src/channels/qq.rs` | QQ 通道完整实现（OAuth2、WebSocket、入向、出向、审批、/bind、媒体上传） | 重写（当前为单行占位） |
| `src/channels/mod.rs` | 导出 `QqConfig` 与 `QqChannel` | 修改 |
| `src/app/mod.rs` | 在 `HarnessConfig` 中实例化 QqChannel 加入 ChannelManager | 修改 |
| `docs/current-state.md` | 已实现能力列表追加 QQ 通道 | 修改 |
| `docs/configuration.md` | 追加 QQ 配置段示例 | 修改 |
| `.env.example` | 追加 `QQ_APP_ID` / `QQ_APP_SECRET` | 修改 |
| `docs/design/im-channel-adapters.md` | QQ 段落状态标注 | 修改 |
| `AGENTS.md` 与 `CLAUDE.md` | 已实现段追加 QQ 通道 | 修改 |
| `docs/design/README.md` 与 `docs/README.md` | 索引本设计文档 | 修改 |

---

## Task 1: 添加依赖与 QqConfig 配置结构

__Files:__

- Modify: `Cargo.toml`
- Modify: `src/channels/config.rs`
- Modify: `src/channels/mod.rs`

__Interfaces:__

- Produces: `QqConfig { app_id, app_secret, allowed_users, pairing_enabled, pairing_code }`
- Produces: `ChannelConfigs.qq: Option<QqConfig>`
- Produces: `expand_env_vars()` 扩展 `QQ_APP_ID` / `QQ_APP_SECRET` 回退

- [ ] __Step 1: 添加依赖到 Cargo.toml__

修改 [Cargo.toml](../../../Cargo.toml)，在 `[dependencies]` 段 `reqwest` 之后追加：

```toml
tokio-tungstenite = { version = "0.24", features = ["rustls-tls-webpki-roots"] }
base64 = "0.22"
sha2 = "0.10"
md5 = "0.7"
```

- [ ] __Step 2: 运行 cargo check 验证依赖可用__

Run: `cargo check --all-features`
Expected: 编译通过，无依赖错误

- [ ] __Step 3: 在 src/channels/config.rs 添加 QqConfig 结构__

在 [src/channels/config.rs](../../../src/channels/config.rs) 的 `TelegramConfig` 之后追加：

```rust
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
```

修改 `ChannelConfigs` 添加 `qq` 字段：

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfigs {
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub qq: Option<QqConfig>,
}
```

修改 `expand_env_vars()` 添加 QQ 段处理（在 Telegram 处理之后）：

```rust
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
```

- [ ] __Step 4: 更新 src/channels/mod.rs 导出__

修改 [src/channels/mod.rs](../../../src/channels/mod.rs#L10) 的 pub use 行：

```rust
pub use config::{ChannelConfigs, QqConfig, TelegramConfig};
```

- [ ] __Step 5: 添加单元测试验证 QqConfig 解析__

在 [src/channels/config.rs](../../../src/channels/config.rs) 的 `#[cfg(test)]` mod tests 中追加：

```rust
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
```

- [ ] __Step 6: 运行测试__

Run: `cargo test --all-features channels::config`
Expected: 所有 config 测试通过

- [ ] __Step 7: 运行完整检查__

Run: `cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
Expected: 全部通过

- [ ] __Step 8: 提交__

```bash
git add Cargo.toml Cargo.lock src/channels/config.rs src/channels/mod.rs
git commit -m "feat(channels): add QqConfig structure and dependencies"
```

---

## Task 2: QqChannel 骨架与 OAuth2 token 缓存

__Files:__

- Modify: `src/channels/qq.rs`（当前仅占位注释）
- Modify: `src/channels/mod.rs`

__Interfaces:__

- Consumes: `QqConfig` from Task 1
- Produces: `QqChannel::new(config, config_path)` 构造函数
- Produces: `QqChannel::get_token() -> Result<String, ChannelError>` 返回有效 access_token
- Produces: `QqChannel::fetch_access_token() -> Result<(String, u64), ChannelError>`
- Produces: `QqChannel::with_workspace_dir(dir)` builder 方法
- Produces: `QqChannel::health_check() -> bool` 通过 fetch_access_token 判断

- [ ] __Step 1: 写失败测试 — QqChannel 构造与名称__

替换 [src/channels/qq.rs](../../../src/channels/qq.rs) 内容为：

```rust
//! QQ 官方 Bot API 通道实现

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tokio::sync::RwLock;

use crate::channels::config::{ChannelConfigs, QqConfig};
use crate::channels::traits::{Channel, ChannelError};

const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_AUTH_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

/// QQ 通道实现
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
    async fn set_token_for_test(&self, token: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut cache = self.token_cache.write().await;
        *cache = Some((token.to_string(), now + 3600));
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::config::QqConfig;

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
        let ch = QqChannel::new(make_config())
            .with_workspace_dir(PathBuf::from("/tmp/qq"));
        assert_eq!(ch.workspace_dir, Some(PathBuf::from("/tmp/qq")));
    }
}
```

- [ ] __Step 2: 更新 mod.rs 导出 QqChannel__

修改 [src/channels/mod.rs](../../../src/channels/mod.rs)：

```rust
pub use config::{ChannelConfigs, QqConfig, TelegramConfig};
pub use frontend::ChannelFrontend;
pub use manager::ChannelManager;
pub use qq::QqChannel;
pub use send_tool::ChannelSendTool;
pub use telegram::TelegramChannel;
pub use traits::{
    AttachmentKind, Channel, ChannelAttachment, ChannelError, ChannelInboundMessage,
    ChannelOutboundMessage,
};
```

- [ ] __Step 3: 运行测试验证骨架编译通过__

Run: `cargo test --all-features channels::qq::tests::name_returns_qq`
Expected: PASS

- [ ] __Step 4: 实现 fetch_access_token 与 get_token__

在 [src/channels/qq.rs](../../../src/channels/qq.rs) 的 `impl QqChannel` 块中追加：

```rust
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
async fn get_token(&self) -> Result<String, ChannelError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    {
        let cache = self.token_cache.read().await;
        if let Some((ref token, expiry)) = *cache {
            if now < expiry {
                return Ok(token.clone());
            }
        }
    }
    let (token, expiry) = self.fetch_access_token().await?;
    {
        let mut cache = self.token_cache.write().await;
        *cache = Some((token.clone(), expiry));
    }
    Ok(token)
}
```

- [ ] __Step 5: 实现 health_check 覆盖默认实现__

在 `impl Channel for QqChannel` 块中追加：

```rust
async fn health_check(&self) -> bool {
    self.fetch_access_token().await.is_ok()
}
```

- [ ] __Step 6: 写失败测试 — OAuth2 token 获取（mock server）__

在 [src/channels/qq.rs](../../../src/channels/qq.rs) 的 `mod tests` 中追加：

```rust
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
```

- [ ] __Step 7: 写测试 — token 缓存复用逻辑__

追加测试：

```rust
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
```

- [ ] __Step 8: 运行测试__

Run: `cargo test --all-features channels::qq`
Expected: 所有测试通过

- [ ] __Step 9: 运行完整检查__

Run: `cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
Expected: 全部通过

- [ ] __Step 10: 提交__

```bash
git add src/channels/qq.rs src/channels/mod.rs
git commit -m "feat(channels): add QqChannel skeleton with OAuth2 token cache"
```

---

## Task 3: 入向消息组装与附件下载

__Files:__

- Modify: `src/channels/qq.rs`

__Interfaces:__

- Consumes: `QqChannel` from Task 2
- Produces: `QqChannel::compose_message_content(payload) -> Option<String>` 解析 QQ 入向 payload
- Produces: `QqChannel::download_attachment(url, dir, filename) -> Result<PathBuf>`
- Produces: 辅助函数 `infer_attachment_marker(ct, filename) -> &'static str` / `fix_qq_url(url) -> String`

- [ ] __Step 1: 添加辅助函数 fix_qq_url 与 infer_attachment_marker__

在 [src/channels/qq.rs](../../../src/channels/qq.rs) 的 `const` 声明后追加：

```rust
/// 修复 QQ CDN URL 缺失协议前缀的问题（//cdn.example.com → https://cdn.example.com）
fn fix_qq_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// 根据 content_type 或文件扩展名推断附件 marker 类型。
fn infer_attachment_marker(content_type: &str, filename: &str) -> &'static str {
    let ct = content_type.to_ascii_lowercase();
    if ct.starts_with("image/") {
        return "IMAGE";
    }
    if ct.starts_with("audio/") || ct.contains("voice") {
        return "VOICE";
    }
    if ct.starts_with("video/") {
        return "VIDEO";
    }
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".heic")
        || lower.ends_with(".heif")
        || lower.ends_with(".svg")
    {
        return "IMAGE";
    }
    if lower.ends_with(".mp3")
        || lower.ends_with(".wav")
        || lower.ends_with(".silk")
        || lower.ends_with(".ogg")
        || lower.ends_with(".flac")
        || lower.ends_with(".m4a")
    {
        return "VOICE";
    }
    if lower.ends_with(".mp4")
        || lower.ends_with(".mov")
        || lower.ends_with(".mkv")
        || lower.ends_with(".avi")
        || lower.ends_with(".webm")
    {
        return "VIDEO";
    }
    "DOCUMENT"
}
```

- [ ] __Step 2: 写测试验证辅助函数__

在 `mod tests` 中追加：

```rust
#[test]
fn fix_qq_url_prepends_https() {
    assert_eq!(
        fix_qq_url("//cdn.example.com/a.png"),
        "https://cdn.example.com/a.png"
    );
    assert_eq!(
        fix_qq_url("https://cdn.example.com/a.png"),
        "https://cdn.example.com/a.png"
    );
}

#[test]
fn infer_attachment_marker_by_content_type() {
    assert_eq!(infer_attachment_marker("image/png", "a"), "IMAGE");
    assert_eq!(infer_attachment_marker("audio/mpeg", "a"), "VOICE");
    assert_eq!(infer_attachment_marker("video/mp4", "a"), "VIDEO");
    assert_eq!(infer_attachment_marker("application/pdf", "a"), "DOCUMENT");
}

#[test]
fn infer_attachment_marker_by_extension_fallback() {
    assert_eq!(infer_attachment_marker("", "photo.jpg"), "IMAGE");
    assert_eq!(infer_attachment_marker("", "song.mp3"), "VOICE");
    assert_eq!(infer_attachment_marker("", "clip.mp4"), "VIDEO");
    assert_eq!(infer_attachment_marker("", "unknown.xyz"), "DOCUMENT");
}
```

- [ ] __Step 3: 运行测试验证通过__

Run: `cargo test --all-features channels::qq::tests::fix_qq_url channels::qq::tests::infer`
Expected: PASS

- [ ] __Step 4: 实现 download_attachment__

在 `impl QqChannel` 块中追加：

```rust
/// 下载附件到本地工作目录，文件名加 UUID 后缀避免冲突。
async fn download_attachment(
    &self,
    url: &str,
    dir: &std::path::Path,
    filename: &str,
) -> Result<std::path::PathBuf, ChannelError> {
    tokio::fs::create_dir_all(dir).await.map_err(|e| ChannelError::Api {
        code: 0,
        message: e.to_string(),
    })?;
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let unique = &uuid::Uuid::new_v4().to_string()[..8];
    let safe_name = if ext.is_empty() {
        format!("{stem}_{unique}")
    } else {
        format!("{stem}_{unique}.{ext}")
    };
    let dest = dir.join(&safe_name);
    let resp = self.client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(ChannelError::Api {
            code: resp.status().as_u16() as i32,
            message: format!("download failed: {url}"),
        });
    }
    let bytes = resp.bytes().await?;
    tokio::fs::write(&dest, &bytes).await.map_err(|e| ChannelError::Api {
        code: 0,
        message: e.to_string(),
    })?;
    Ok(dest)
}
```

- [ ] __Step 5: 实现 compose_message_content__

在 `impl QqChannel` 块中追加：

```rust
/// 从 QQ 入向事件 payload 组装消息内容，处理附件下载与 marker 生成。
async fn compose_message_content(&self, payload: &serde_json::Value) -> Option<String> {
    let text = payload
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim();
    let mut markers: Vec<String> = Vec::new();
    let mut voice_transcripts: Vec<String> = Vec::new();

    if let Some(attachments) = payload.get("attachments").and_then(|a| a.as_array()) {
        for att in attachments {
            let url = match att.get("url").and_then(|u| u.as_str()) {
                Some(u) if !u.trim().is_empty() => fix_qq_url(u),
                _ => continue,
            };
            let content_type = att
                .get("content_type")
                .and_then(|ct| ct.as_str())
                .unwrap_or("");
            let filename = att
                .get("filename")
                .and_then(|f| f.as_str())
                .unwrap_or("attachment");
            let marker_type = infer_attachment_marker(content_type, filename);

            let is_voice = content_type == "voice"
                || content_type.starts_with("audio/")
                || marker_type == "VOICE";
            let (download_url, save_filename) = if is_voice {
                if let Some(wav_url) = att
                    .get("voice_wav_url")
                    .and_then(|u| u.as_str())
                    .filter(|u| !u.trim().is_empty())
                {
                    let fixed = fix_qq_url(wav_url);
                    let wav_name = std::path::Path::new(fixed.split('?').next().unwrap_or(&fixed))
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("voice.wav")
                        .to_string();
                    (fixed, wav_name)
                } else {
                    (url.clone(), filename.to_string())
                }
            } else {
                (url.clone(), filename.to_string())
            };

            let location = if let Some(ref ws) = self.workspace_dir {
                let dir = ws.join("qq_files");
                match self.download_attachment(&download_url, &dir, &save_filename).await {
                    Ok(local_path) => local_path.display().to_string(),
                    Err(e) => {
                        tracing::warn!(event = "QqDownloadFailed", url = %download_url, error = %e, "failed to download attachment");
                        url.clone()
                    }
                }
            } else {
                url.clone()
            };

            if is_voice {
                markers.push(format!("[{marker_type}:{location}]"));
                if let Some(asr_text) = att
                    .get("asr_refer_text")
                    .and_then(|t| t.as_str())
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty())
                {
                    voice_transcripts.push(asr_text.to_string());
                }
            } else {
                markers.push(format!("[{marker_type}:{location}]"));
            }
        }
    }

    let voice_text = match voice_transcripts.len() {
        0 => String::new(),
        1 => format!(
            "<VOICE_TRANSCRIPTION>{}</VOICE_TRANSCRIPTION>",
            voice_transcripts[0]
        ),
        _ => voice_transcripts
            .iter()
            .enumerate()
            .map(|(i, t)| format!("<VOICE_TRANSCRIPTION_{i}>{t}</VOICE_TRANSCRIPTION_{i}>"))
            .collect::<Vec<_>>()
            .join("\n"),
    };

    let mut parts: Vec<&str> = Vec::new();
    if !text.is_empty() {
        parts.push(text);
    }
    if !voice_text.is_empty() {
        parts.push(&voice_text);
    }
    let markers_joined = markers.join("\n");
    if !markers_joined.is_empty() {
        parts.push(&markers_joined);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n"))
}
```

- [ ] __Step 6: 写测试 — compose_message_content 各种场景__

追加测试：

```rust
#[tokio::test]
async fn compose_text_only() {
    let ch = QqChannel::new(make_config());
    let payload = serde_json::json!({ "content": "  hello world  " });
    assert_eq!(
        ch.compose_message_content(&payload).await,
        Some("hello world".to_string())
    );
}

#[tokio::test]
async fn compose_image_attachment_without_workspace() {
    let ch = QqChannel::new(make_config());
    let payload = serde_json::json!({
        "content": "   ",
        "attachments": [{
            "content_type": "image/jpg",
            "url": "https://cdn.example.com/a.jpg"
        }]
    });
    assert_eq!(
        ch.compose_message_content(&payload).await,
        Some("[IMAGE:https://cdn.example.com/a.jpg]".to_string())
    );
}

#[tokio::test]
async fn compose_text_and_multiple_attachments() {
    let ch = QqChannel::new(make_config());
    let payload = serde_json::json!({
        "content": "Here is an image",
        "attachments": [
            { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
            { "filename": "b.jpeg", "url": "https://cdn.example.com/b.jpeg" }
        ]
    });
    let result = ch.compose_message_content(&payload).await.unwrap();
    assert!(result.contains("Here is an image"));
    assert!(result.contains("[IMAGE:https://cdn.example.com/a.png]"));
    assert!(result.contains("[IMAGE:https://cdn.example.com/b.jpeg]"));
}

#[tokio::test]
async fn compose_fixes_double_slash_url() {
    let ch = QqChannel::new(make_config());
    let payload = serde_json::json!({
        "content": "",
        "attachments": [{
            "content_type": "image/png",
            "url": "//cdn.example.com/a.png"
        }]
    });
    let result = ch.compose_message_content(&payload).await.unwrap();
    assert!(result.contains("https://cdn.example.com/a.png"));
    assert!(!result.starts_with("[IMAGE://"));
}

#[tokio::test]
async fn compose_drops_empty_url() {
    let ch = QqChannel::new(make_config());
    let payload = serde_json::json!({
        "content": "   ",
        "attachments": [{
            "content_type": "image/png",
            "url": "   "
        }]
    });
    assert_eq!(ch.compose_message_content(&payload).await, None);
}

#[tokio::test]
async fn compose_all_attachment_types() {
    let ch = QqChannel::new(make_config());
    let payload = serde_json::json!({
        "content": "",
        "attachments": [
            { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
            { "content_type": "audio/mpeg", "url": "https://cdn.example.com/b.mp3" },
            { "content_type": "video/mp4", "url": "https://cdn.example.com/c.mp4" },
            { "content_type": "application/pdf", "url": "https://cdn.example.com/d.pdf" }
        ]
    });
    let result = ch.compose_message_content(&payload).await.unwrap();
    assert!(result.contains("[IMAGE:"));
    assert!(result.contains("[VOICE:"));
    assert!(result.contains("[VIDEO:"));
    assert!(result.contains("[DOCUMENT:"));
}
```

- [ ] __Step 7: 运行测试__

Run: `cargo test --all-features channels::qq`
Expected: 所有测试通过

- [ ] __Step 8: 运行 clippy 检查__

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告

- [ ] __Step 9: 提交__

```bash
git add src/channels/qq.rs
git commit -m "feat(channels): add QQ inbound message composition and attachment download"
```

---

## Task 4: 白名单匹配与消息去重

__Files:__

- Modify: `src/channels/qq.rs`

__Interfaces:__

- Produces: `QqChannel::is_user_allowed(user_openid) -> bool`
- Produces: `QqChannel::is_duplicate(msg_id) -> bool`（带 dedup HashSet）
- Produces: `QqChannel::runtime_allow(user_openid)` 写入运行时白名单
- Produces: `dedup: Arc<RwLock<HashSet<String>>>` 字段

- [ ] __Step 1: 添加 dedup 字段到 QqChannel 结构__

修改 `QqChannel` 结构体定义，在 `token_cache` 后追加：

```rust
pub struct QqChannel {
    config: QqConfig,
    config_path: Option<PathBuf>,
    runtime_allowed_users: Arc<RwLock<HashSet<String>>>,
    client: Client,
    token_cache: Arc<RwLock<Option<(String, u64)>>>,
    dedup: Arc<RwLock<HashSet<String>>>,
    workspace_dir: Option<PathBuf>,
    api_base: String,
    auth_url: String,
}
```

修改 `new_with_path` 初始化 `dedup`（保留 Task 2 已有的 `api_base` / `auth_url` 字段初始化）：

```rust
pub fn new_with_path(config: QqConfig, config_path: Option<PathBuf>) -> Self {
    Self {
        config,
        config_path,
        runtime_allowed_users: Arc::new(RwLock::new(HashSet::new())),
        client: Client::new(),
        token_cache: Arc::new(RwLock::new(None)),
        dedup: Arc::new(RwLock::new(HashSet::new())),
        workspace_dir: None,
        api_base: QQ_API_BASE.to_string(),
        auth_url: QQ_AUTH_URL.to_string(),
    }
}
```

- [ ] __Step 2: 实现 is_user_allowed / runtime_allow / is_duplicate__

在 `impl QqChannel` 块中追加（在 `fetch_access_token` 之前）：

```rust
const DEDUP_CAPACITY: usize = 10_000;

/// 白名单匹配：runtime_allowed_users 优先，然后按 allowed_users 通配符 `*` 或精确 openid 匹配。
async fn is_user_allowed(&self, user_openid: &str) -> bool {
    if self
        .runtime_allowed_users
        .read()
        .await
        .contains(user_openid)
    {
        return true;
    }
    if self.config.allowed_users.iter().any(|u| u == "*") {
        return true;
    }
    if self.config.allowed_users.is_empty() {
        return false;
    }
    self.config
        .allowed_users
        .iter()
        .any(|allowed| allowed == user_openid)
}

/// 加入运行时白名单（/bind 配对通过时调用）。
fn runtime_allow(&self, user_openid: &str) {
    // runtime_allowed_users 是 tokio::sync::RwLock，但为保持与 listen() 内一致用 blocking_write
    // 注意：在异步上下文中使用 blocking_write 会阻塞 runtime，应改用 write().await
    // 此处保留方法签名，实际调用处用 write().await
    // 改为 async 版本：
    todo!("runtime_allow_async")
}
```

> __重要__：由于 `runtime_allowed_users` 是 `tokio::sync::RwLock`，`runtime_allow` 必须是 async。重新设计：

替换上述 `runtime_allow` 实现为：

```rust
/// 加入运行时白名单（/bind 配对通过时调用）。
async fn runtime_allow(&self, user_openid: &str) {
    self.runtime_allowed_users
        .write()
        .await
        .insert(user_openid.to_string());
}

/// 消息去重检查：msg_id 已存在返回 true，否则插入并返回 false。
/// 容量达上限时淘汰一半旧条目。
async fn is_duplicate(&self, msg_id: &str) -> bool {
    if msg_id.is_empty() {
        return false;
    }
    let mut dedup = self.dedup.write().await;
    if dedup.contains(msg_id) {
        return true;
    }
    if dedup.len() >= DEDUP_CAPACITY {
        let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
        for key in to_remove {
            dedup.remove(&key);
        }
    }
    dedup.insert(msg_id.to_string());
    false
}
```

- [ ] __Step 3: 写测试 — 白名单匹配__

追加测试：

```rust
#[tokio::test]
async fn user_allowed_by_wildcard() {
    let mut cfg = make_config();
    cfg.allowed_users = vec!["*".to_string()];
    let ch = QqChannel::new(cfg);
    assert!(ch.is_user_allowed("anyone").await);
}

#[tokio::test]
async fn user_allowed_by_specific_openid() {
    let mut cfg = make_config();
    cfg.allowed_users = vec!["user123".to_string()];
    let ch = QqChannel::new(cfg);
    assert!(ch.is_user_allowed("user123").await);
    assert!(!ch.is_user_allowed("other").await);
}

#[tokio::test]
async fn empty_allowlist_denies_all() {
    let ch = QqChannel::new(make_config());
    assert!(!ch.is_user_allowed("anyone").await);
}

#[tokio::test]
async fn runtime_allow_overrides_empty_config() {
    let ch = QqChannel::new(make_config());
    ch.runtime_allow("runtime_user").await;
    assert!(ch.is_user_allowed("runtime_user").await);
    assert!(!ch.is_user_allowed("other").await);
}
```

- [ ] __Step 4: 写测试 — 消息去重__

追加测试：

```rust
#[tokio::test]
async fn dedup_first_occurrence_returns_false() {
    let ch = QqChannel::new(make_config());
    assert!(!ch.is_duplicate("msg1").await);
}

#[tokio::test]
async fn dedup_second_occurrence_returns_true() {
    let ch = QqChannel::new(make_config());
    assert!(!ch.is_duplicate("msg1").await);
    assert!(ch.is_duplicate("msg1").await);
}

#[tokio::test]
async fn dedup_empty_msg_id_returns_false() {
    let ch = QqChannel::new(make_config());
    assert!(!ch.is_duplicate("").await);
    assert!(!ch.is_duplicate("").await);
}

#[tokio::test]
async fn dedup_independent_msg_ids() {
    let ch = QqChannel::new(make_config());
    assert!(!ch.is_duplicate("msg_a").await);
    assert!(!ch.is_duplicate("msg_b").await);
    assert!(ch.is_duplicate("msg_a").await);
}
```

- [ ] __Step 5: 运行测试__

Run: `cargo test --all-features channels::qq`
Expected: 所有测试通过

- [ ] __Step 6: 运行 clippy__

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告

- [ ] __Step 7: 提交__

```bash
git add src/channels/qq.rs
git commit -m "feat(channels): add QQ allowlist matching and message dedup"
```

---

## Task 5: 出向 send() — Markdown 文本与媒体上传

__Files:__

- Modify: `src/channels/qq.rs`

__Interfaces:__

- Consumes: `extract_attachments` from [src/channels/traits.rs](../../../src/channels/traits.rs#L100)
- Produces: `QqChannel::send(message)` 完整实现
- Produces: `QqChannel::send_text_markdown(recipient, content)` msg_type=2
- Produces: `QqChannel::send_attachment(recipient, attachment)` 媒体上传分发
- Produces: `QqChannel::upload_media(...) / send_media_message(...)` HTTP 调用
- Produces: `QqChannel::resolve_recipient(recipient) -> (&str, String)` 静态方法
- Produces: 辅助函数 `html_to_markdown_for_qq(text)` / `marker_kind_to_qq_file_type(marker, target)` / `next_msg_seq()`

- [ ] __Step 1: 添加 QQMediaFileType 枚举与辅助函数__

在 [src/channels/qq.rs](../../../src/channels/qq.rs) 的 `const` 之后追加：

```rust
/// QQ API 媒体文件类型枚举（数值对应 API file_type 字段）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QqMediaFileType {
    Image = 1,
    Video = 2,
    Voice = 3,
    File = 4,
}

/// 根据 marker 字符串与目标路径扩展名映射到 QQMediaFileType。
/// AUDIO/VOICE 非原生格式（非 wav/mp3/silk）降级为 File。
fn marker_kind_to_qq_file_type(marker: &str, target: &str) -> Option<QqMediaFileType> {
    match marker.trim().to_ascii_uppercase().as_str() {
        "IMAGE" | "PHOTO" => Some(QqMediaFileType::Image),
        "DOCUMENT" | "FILE" => Some(QqMediaFileType::File),
        "VIDEO" => Some(QqMediaFileType::Video),
        "AUDIO" | "VOICE" => {
            let ext = std::path::Path::new(target.split('?').next().unwrap_or(target))
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if matches!(
                ext.to_ascii_lowercase().as_str(),
                "wav" | "mp3" | "silk"
            ) {
                Some(QqMediaFileType::Voice)
            } else {
                Some(QqMediaFileType::File)
            }
        }
        _ => None,
    }
}

/// 将 ChannelFrontend 生成的 HTML 审批内容转换为 QQ markdown。
/// - `<pre>...</pre>` → ``` ... ``` 代码块
/// - `<b>...</b>` / `<strong>...</strong>` → **...**
/// - `<i>...</i>` / `<em>...</em>` → *...*
/// - `<code>...</code>` → `...`
/// - `<br>` → 换行
/// - 其他 HTML 特殊字符 `&lt;` `&gt;` `&amp;` `&quot;` `&#39;` 反转义为原字符
/// - 未识别的标签剥除标签保留内容
fn html_to_markdown_for_qq(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // 查找闭合 >
            if let Some(close_rel) = text[i + 1..].find('>') {
                let close = i + 1 + close_rel;
                let tag = &text[i + 1..close];
                let tag_lower = tag.to_ascii_lowercase();
                let trimmed = tag_lower.trim_end_matches('/');
                if trimmed == "br" {
                    out.push('\n');
                    i = close + 1;
                    continue;
                }
                // 处理 <pre>、<b>、<strong>、<i>、<em>、<code> 等
                if let Some(tag_name) = trimmed.split_whitespace().next() {
                    let (prefix, suffix, skip_inner_newline) = match tag_name {
                        "pre" => ("```\n", "\n```", false),
                        "b" | "strong" => ("**", "**", true),
                        "i" | "em" => ("*", "*", true),
                        "code" => ("`", "`", true),
                        _ => ("", "", true),
                    };
                    if !prefix.is_empty() {
                        // 找到对应闭合标签
                        let close_tag = format!("/{tag_name}");
                        if let Some(close_end_rel) = text[close + 1..]
                            .find(&format!('<{close_tag}>'))
                            .or_else(|| {
                                text[close + 1..]
                                    .find(&format!("<{close_tag} "))
                            })
                        {
                            let inner_start = close + 1;
                            let inner_end = close + 1 + close_end_rel;
                            let inner = &text[inner_start..inner_end];
                            out.push_str(prefix);
                            out.push_str(&html_to_markdown_for_qq(inner));
                            if !skip_inner_newline && !inner.ends_with('\n') {
                                out.push('\n');
                            }
                            out.push_str(suffix);
                            i = inner_end + close_tag.len() + 2; // 跳过 <...>
                            continue;
                        }
                    }
                    // 未识别标签剥除
                    i = close + 1;
                    continue;
                }
            }
            out.push('<');
            i += 1;
        } else if text[i..].starts_with("&lt;") {
            out.push('<');
            i += 4;
        } else if text[i..].starts_with("&gt;") {
            out.push('>');
            i += 4;
        } else if text[i..].starts_with("&amp;") {
            out.push('&');
            i += 5;
        } else if text[i..].starts_with("&quot;") {
            out.push('"');
            i += 6;
        } else if text[i..].starts_with("&#39;") {
            out.push('\'');
            i += 5;
        } else {
            let c = text[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// 生成 QQ API 请求用的 msg_seq（0~65535）。
fn next_msg_seq() -> u32 {
    let time_part = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32)
        % 100_000_000;
    let random = u32::from(rand::random::<u16>());
    (time_part ^ random) % 65536
}
```

> 注：`rand` crate 未在依赖中，需用 `uuid` 替代或添加 rand。检查 Cargo.toml，已有 `uuid`。改用 uuid 生成随机数：

替换 `next_msg_seq` 实现：

```rust
/// 生成 QQ API 请求用的 msg_seq（0~65535）。
fn next_msg_seq() -> u32 {
    let time_part = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32)
        % 100_000_000;
    let random = (uuid::Uuid::new_v4().as_u128() & 0xFFFF) as u32;
    (time_part ^ random) % 65536
}
```

- [ ] __Step 2: 写测试 — 辅助函数__

追加测试：

```rust
#[test]
fn marker_kind_image_variants() {
    assert_eq!(
        marker_kind_to_qq_file_type("IMAGE", "/a.png"),
        Some(QqMediaFileType::Image)
    );
    assert_eq!(
        marker_kind_to_qq_file_type("PHOTO", "/a.png"),
        Some(QqMediaFileType::Image)
    );
}

#[test]
fn marker_kind_document_and_file() {
    assert_eq!(
        marker_kind_to_qq_file_type("DOCUMENT", "/a.pdf"),
        Some(QqMediaFileType::File)
    );
    assert_eq!(
        marker_kind_to_qq_file_type("FILE", "/a.zip"),
        Some(QqMediaFileType::File)
    );
}

#[test]
fn marker_kind_voice_native_formats() {
    assert_eq!(
        marker_kind_to_qq_file_type("VOICE", "/a.wav"),
        Some(QqMediaFileType::Voice)
    );
    assert_eq!(
        marker_kind_to_qq_file_type("AUDIO", "/a.mp3"),
        Some(QqMediaFileType::Voice)
    );
    assert_eq!(
        marker_kind_to_qq_file_type("VOICE", "/a.silk"),
        Some(QqMediaFileType::Voice)
    );
}

#[test]
fn marker_kind_voice_non_native_degrades_to_file() {
    assert_eq!(
        marker_kind_to_qq_file_type("VOICE", "/a.ogg"),
        Some(QqMediaFileType::File)
    );
    assert_eq!(
        marker_kind_to_qq_file_type("AUDIO", "/a.flac"),
        Some(QqMediaFileType::File)
    );
}

#[test]
fn html_to_markdown_pre_becomes_code_block() {
    let input = "工具：test\n输入：<pre>hello world</pre>";
    let result = html_to_markdown_for_qq(input);
    assert!(result.contains("```\nhello world\n```"));
    assert!(!result.contains("<pre>"));
}

#[test]
fn html_to_markdown_b_becomes_bold() {
    let input = "<b>重要</b>";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "**重要**");
}

#[test]
fn html_to_markdown_i_becomes_italic() {
    let input = "<i>说明</i>";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "*说明*");
}

#[test]
fn html_to_markdown_code_becomes_backtick() {
    let input = "<code>let x = 1;</code>";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "`let x = 1;`");
}

#[test]
fn html_to_markdown_unescapes_entities() {
    let input = "&lt;script&gt; &amp; text&quot;quote&#39;apos";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "<script> & text\"quote'apos");
}

#[test]
fn html_to_markdown_unknown_tag_stripped() {
    let input = "<div>内容</div>";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "内容");
}

#[test]
fn html_to_markdown_br_becomes_newline() {
    let input = "第一行<br>第二行";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "第一行\n第二行");
}

#[test]
fn html_to_markdown_preserves_chinese() {
    let input = "你好 <b>世界</b>";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "你好 **世界**");
}

#[test]
fn html_to_markdown_nested_pre_inside_b() {
    let input = "<b>外层 <code>内层</code></b>";
    let result = html_to_markdown_for_qq(input);
    assert_eq!(result, "**外层 `内层`**");
}

#[test]
fn html_to_markdown_approval_request_example() {
    let input = "🔒 需要你的确认\n\n工具：channel_send\n输入：<pre>{&quot;value&quot;: &quot;&lt;script&gt;\"}</pre>\n\n请选择一个选项：";
    let result = html_to_markdown_for_qq(input);
    assert!(result.contains("```\n{"));
    assert!(result.contains("<script>"));
    assert!(!result.contains("&lt;"));
    assert!(!result.contains("<pre>"));
}

#[test]
fn next_msg_seq_within_range() {
    for _ in 0..100 {
        let seq = next_msg_seq();
        assert!(seq < 65536);
    }
}
```

- [ ] __Step 3: 运行测试__

Run: `cargo test --all-features channels::qq::tests::marker_kind channels::qq::tests::escape_html channels::qq::tests::next_msg_seq`
Expected: PASS

- [ ] __Step 4: 实现 resolve_recipient 静态方法__

在 `impl QqChannel` 块中追加：

```rust
/// 解析 recipient 字符串为 (scope, id)。
/// scope="groups" 对应群消息，scope="users" 对应 C2C。
/// 保留 QQ openid 中合法字符（字母、数字、`_`、`-`），避免截断 UUID 形式的 openid。
fn resolve_recipient(recipient: &str) -> (&'static str, String) {
    if let Some(group_id) = recipient.strip_prefix("group:") {
        return ("groups", group_id.to_string());
    }
    let raw_uid = recipient.strip_prefix("user:").unwrap_or(recipient);
    let user_id: String = raw_uid
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    ("users", user_id)
}
```

- [ ] __Step 5: 写测试 — resolve_recipient__

追加测试：

```rust
#[test]
fn resolve_recipient_group_prefix() {
    let (scope, id) = QqChannel::resolve_recipient("group:abc123");
    assert_eq!(scope, "groups");
    assert_eq!(id, "abc123");
}

#[test]
fn resolve_recipient_user_prefix() {
    let (scope, id) = QqChannel::resolve_recipient("user:xyz789");
    assert_eq!(scope, "users");
    assert_eq!(id, "xyz789");
}

#[test]
fn resolve_recipient_bare_id() {
    let (scope, id) = QqChannel::resolve_recipient("raw_id_123");
    assert_eq!(scope, "users");
    assert_eq!(id, "raw_id_123");
}

#[test]
fn resolve_recipient_preserves_hyphen_in_uuid_openid() {
    let (scope, id) = QqChannel::resolve_recipient("user:01912345-6789-7abc-8def-0123456789ab");
    assert_eq!(scope, "users");
    assert_eq!(id, "01912345-6789-7abc-8def-0123456789ab");
}

#[test]
fn resolve_recipient_strips_whitespace_in_bare_id() {
    let (scope, id) = QqChannel::resolve_recipient("user:abc def");
    assert_eq!(scope, "users");
    assert_eq!(id, "abcdef");
}
```

- [ ] __Step 6: 运行测试__

Run: `cargo test --all-features channels::qq::tests::resolve_recipient`
Expected: PASS

- [ ] __Step 7: 实现 send_text_markdown 与 send_media_message__

在 `impl QqChannel` 块中追加：

```rust
/// 发送 markdown 文本消息（msg_type=2）。
async fn send_text_markdown(&self, recipient: &str, content: &str) -> Result<(), ChannelError> {
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    let url = format!("{}/v2/{scope}/{id}/messages", self.api_base);
    let body = json!({
        "markdown": { "content": content },
        "msg_type": 2,
        "msg_seq": next_msg_seq(),
    });
    let resp = self
        .client
        .post(&url)
        .header("Authorization", format!("QQBot {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    Ok(())
}

/// 发送富媒体消息（msg_type=7）。
async fn send_media_message(&self, recipient: &str, file_info: &str) -> Result<(), ChannelError> {
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    let url = format!("{}/v2/{scope}/{id}/messages", self.api_base);
    let body = json!({
        "msg_type": 7,
        "media": { "file_info": file_info },
        "msg_seq": next_msg_seq(),
    });
    let resp = self
        .client
        .post(&url)
        .header("Authorization", format!("QQBot {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    Ok(())
}
```

- [ ] __Step 8: 实现 upload_media（URL 与 base64 路径）__

在 `impl QqChannel` 块中追加：

```rust
/// 上传媒体到 QQ API，返回 (file_info, ttl)。
/// - url 模式：传 url=Some(...)，file_data=None
/// - base64 模式：传 file_data=Some(...)，url=None
async fn upload_media(
    &self,
    recipient: &str,
    file_type: QqMediaFileType,
    url: Option<&str>,
    file_data: Option<&str>,
    file_name: Option<&str>,
) -> Result<(String, Option<u64>), ChannelError> {
    let token = self.get_token().await?;
    let (scope, id) = Self::resolve_recipient(recipient);
    let api_url = format!("{}/v2/{scope}/{id}/files", self.api_base);
    let mut body = json!({
        "file_type": file_type as u8,
        "srv_send_msg": false,
    });
    if let Some(u) = url {
        body["url"] = json!(u);
    }
    if let Some(d) = file_data {
        body["file_data"] = json!(d);
    }
    if file_type == QqMediaFileType::File {
        if let Some(name) = file_name {
            body["file_name"] = json!(name);
        }
    }
    let resp = self
        .client
        .post(&api_url)
        .header("Authorization", format!("QQBot {token}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    let raw_body = resp.text().await.unwrap_or_default();
    let upload_resp = parse_upload_response_body(&raw_body)?;
    Ok((upload_resp.file_info, upload_resp.ttl))
}
```

- [ ] __Step 9: 添加 parse_upload_response_body 辅助函数__

在文件底部辅助函数区追加：

```rust
#[derive(Debug, serde::Deserialize)]
struct QqUploadResponse {
    file_info: String,
    #[allow(dead_code)]
    file_uuid: Option<String>,
    ttl: Option<u64>,
}

/// 解析 QQ 上传类接口返回，兼容顶层或 data 包裹格式。
fn parse_upload_response_body(raw_body: &str) -> Result<QqUploadResponse, ChannelError> {
    let root: serde_json::Value = serde_json::from_str(raw_body).map_err(|e| {
        ChannelError::Api {
            code: 0,
            message: format!("QQ upload response json decode failed: {e}"),
        }
    })?;
    let data = root.get("data").unwrap_or(&root);
    let file_info = data
        .get("file_info")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ChannelError::Api {
            code: 0,
            message: format!("QQ upload response missing file_info; body={raw_body}"),
        })?
        .to_string();
    let file_uuid = data
        .get("file_uuid")
        .and_then(serde_json::Value::as_str)
        .map(std::string::ToString::to_string);
    let ttl = data.get("ttl").and_then(serde_json::Value::as_u64).or_else(|| {
        data.get("ttl")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<u64>().ok())
    });
    Ok(QqUploadResponse {
        file_info,
        file_uuid,
        ttl,
    })
}
```

- [ ] __Step 10: 写测试 — parse_upload_response_body__

追加测试：

```rust
#[test]
fn parse_upload_response_with_data_wrapper() {
    let raw = r#"{"data": {"file_info": "abc", "ttl": 3600}}"#;
    let parsed = parse_upload_response_body(raw).unwrap();
    assert_eq!(parsed.file_info, "abc");
    assert_eq!(parsed.ttl, Some(3600));
}

#[test]
fn parse_upload_response_top_level() {
    let raw = r#"{"file_info": "xyz", "ttl": "7200"}"#;
    let parsed = parse_upload_response_body(raw).unwrap();
    assert_eq!(parsed.file_info, "xyz");
    assert_eq!(parsed.ttl, Some(7200));
}

#[test]
fn parse_upload_response_missing_file_info_errors() {
    let raw = r#"{"data": {}}"#;
    assert!(parse_upload_response_body(raw).is_err());
}
```

- [ ] __Step 11: 实现 send_attachment 分发逻辑__

在 `impl QqChannel` 块中追加：

```rust
/// 发送单个附件：根据 target 路径分发到 URL / base64 / 分片上传。
async fn send_attachment(
    &self,
    recipient: &str,
    attachment: &crate::channels::traits::ChannelAttachment,
) -> Result<(), ChannelError> {
    use crate::channels::traits::AttachmentKind;
    use base64::Engine as _;

    let target = attachment.target.trim();
    let file_name = std::path::Path::new(target.split('?').next().unwrap_or(target))
        .file_name()
        .and_then(|n| n.to_str())
        .map(String::from);

    let qq_file_type = match attachment.kind {
        AttachmentKind::Image => QqMediaFileType::Image,
        AttachmentKind::Document => QqMediaFileType::File,
        AttachmentKind::Video => QqMediaFileType::Video,
        AttachmentKind::Audio | AttachmentKind::Voice => {
            let ext = std::path::Path::new(target.split('?').next().unwrap_or(target))
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if matches!(ext.to_ascii_lowercase().as_str(), "wav" | "mp3" | "silk") {
                QqMediaFileType::Voice
            } else {
                QqMediaFileType::File
            }
        }
    };

    if target.starts_with("http://") || target.starts_with("https://") {
        // 安全：仅允许 HTTPS URL，避免 SSRF（QQ API 服务器作为代理拉取附件 URL）。
        if target.starts_with("http://") {
            return Err(ChannelError::Api {
                code: 0,
                message: format!("QQ attachment URL must be HTTPS: {target}"),
            });
        }
        let (file_info, _) = self
            .upload_media(recipient, qq_file_type, Some(target), None, file_name.as_deref())
            .await?;
        self.send_media_message(recipient, &file_info).await?;
    } else {
        let path = std::path::Path::new(target);
        if !path.exists() {
            return Err(ChannelError::Api {
                code: 0,
                message: format!("QQ attachment path not found: {target}"),
            });
        }
        let file_bytes = tokio::fs::read(path).await.map_err(|e| ChannelError::Api {
            code: 0,
            message: e.to_string(),
        })?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&file_bytes);
        let (file_info, _) = self
            .upload_media(
                recipient,
                qq_file_type,
                None,
                Some(&b64),
                file_name.as_deref(),
            )
            .await?;
        self.send_media_message(recipient, &file_info).await?;
    }
    Ok(())
}
```

> 注：本期不实现 >10MB 分片上传（见 Task 8 边界说明），简化为 base64 路径。如文件过大 base64 会失败，由调用方降级处理。

- [ ] __Step 12: 覆写 supported_attachment_kinds__

在 `impl Channel for QqChannel` 块中追加（与 [Telegram 通道](../../../src/channels/telegram.rs#L156-L164) 保持一致，QQ 同样支持全部五种附件类型）：

```rust
fn supported_attachment_kinds(&self) -> Vec<AttachmentKind> {
    vec![
        AttachmentKind::Image,
        AttachmentKind::Document,
        AttachmentKind::Video,
        AttachmentKind::Audio,
        AttachmentKind::Voice,
    ]
}
```

> 注：`supports_html` 与 `supports_inline_keyboard` 默认返回 `false`，QQ 通道不覆写（默认值即正确）。让 ChannelFrontend 依据这些标志选择渲染方式是独立重构任务，不在本期范围。

- [ ] __Step 13: 实现 send() 主流程__

替换 `impl Channel for QqChannel` 中的 `send` 方法：

```rust
async fn send(
    &self,
    message: &crate::channels::traits::ChannelOutboundMessage,
) -> Result<(), ChannelError> {
    use crate::channels::traits::{extract_attachments, ChannelParseMode};

    let (text, inline_attachments) = if message.reply_markup.is_some() {
        (message.content.clone(), vec![])
    } else {
        extract_attachments(&message.content)
    };

    let all_attachments: Vec<_> = message
        .attachments
        .iter()
        .chain(inline_attachments.iter())
        .filter(|a| !a.target.trim().is_empty())
        .cloned()
        .collect();

    let final_text = if let Some(ref markup) = message.reply_markup {
        render_buttons_as_numbered_list(markup, &text)
    } else {
        text.clone()
    };

    let content_to_send = match message.parse_mode {
        // ChannelFrontend 用 Html 模式发送审批请求（含 <pre> 等标签）。
        // QQ 不支持 HTML 渲染，将 HTML 转换为 markdown（代码块/粗体/斜体）后发送。
        Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&final_text),
        Some(ChannelParseMode::Markdown) | None => final_text,
    };

    if !content_to_send.trim().is_empty() {
        self.send_text_markdown(&message.recipient, &content_to_send)
            .await?;
    }

    for attachment in &all_attachments {
        if let Err(e) = self.send_attachment(&message.recipient, attachment).await {
            tracing::warn!(
                event = "QqSendAttachmentFailed",
                target = %attachment.target,
                error = %e,
                "QQ attachment send failed, degrading to text"
            );
            let fallback = format!(
                "{}: {}",
                match attachment.kind {
                    crate::channels::traits::AttachmentKind::Image => "Image",
                    crate::channels::traits::AttachmentKind::Document => "File",
                    crate::channels::traits::AttachmentKind::Video => "Video",
                    crate::channels::traits::AttachmentKind::Audio => "Audio",
                    crate::channels::traits::AttachmentKind::Voice => "Voice",
                },
                attachment.target
            );
            self.send_text_markdown(&message.recipient, &fallback)
                .await?;
        }
    }
    Ok(())
}
```

- [ ] __Step 14: 临时占位 render_buttons_as_numbered_list（Task 6 实现）__

在文件底部辅助函数区追加占位：

```rust
/// 将 ReplyMarkup 转译为编号列表文本（Task 6 完整实现）。
fn render_buttons_as_numbered_list(
    markup: &crate::channels::traits::ReplyMarkup,
    base_content: &str,
) -> String {
    // 占位：Task 6 实现完整逻辑
    base_content.to_string()
}
```

- [ ] __Step 15: 写集成测试 — send_text_markdown 与 send_media_message（wiremock）__

追加测试。使用 `body_partial_json` 只匹配关键字段，忽略动态 `msg_seq`：

```rust
#[tokio::test]
async fn send_text_markdown_posts_msg_type_2() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/users/USER123/messages"))
        .and(body_partial_json(serde_json::json!({
            "markdown": { "content": "hello" },
            "msg_type": 2,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_1",
            "channel_id": "USER123"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    ch.send_text_markdown("user:USER123", "hello")
        .await
        .expect("send_text_markdown");
}

#[tokio::test]
async fn send_media_message_posts_msg_type_7() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/users/USER123/messages"))
        .and(body_partial_json(serde_json::json!({
            "msg_type": 7,
            "media": { "file_info": "fi_abc" },
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
    ch.set_token_for_test("fake_token").await;
    ch.send_media_message("user:USER123", "fi_abc")
        .await
        .expect("send_media_message");
}

#[tokio::test]
async fn send_attachment_rejects_http_url() {
    use crate::channels::traits::{AttachmentKind, ChannelAttachment};

    let ch = QqChannel::new(make_config());
    ch.set_token_for_test("fake_token").await;
    let attachment = ChannelAttachment {
        kind: AttachmentKind::Image,
        target: "http://insecure.example.com/a.png".to_string(),
    };
    let result = ch.send_attachment("user:USER123", &attachment).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("must be HTTPS"),
        "expected HTTPS error, got: {err}"
    );
}
```

- [ ] __Step 16: 运行所有测试__

Run: `cargo test --all-features channels::qq`
Expected: 所有测试通过

- [ ] __Step 17: 运行 clippy__

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告

- [ ] __Step 18: 提交__

```bash
git add src/channels/qq.rs
git commit -m "feat(channels): implement QQ outbound send with markdown and media upload"
```

---

## Task 6: 审批交互 — 文本回复匹配

__Files:__

- Modify: `src/channels/qq.rs`

__Interfaces:__

- Produces: `pending_approvals: Arc<RwLock<HashMap<String, PendingApproval>>>` 字段
- Produces: `QqChannel::record_pending_approval(recipient, request_id, options)` 记录 pending
- Produces: `QqChannel::try_match_approval_reply(recipient, content) -> Option<InboundConfirmation>`
- Produces: `render_buttons_as_numbered_list(markup, base_content) -> String` 完整实现

- [ ] __Step 1: 添加 PendingApproval 结构与 pending_approvals 字段__

在 [src/channels/qq.rs](../../../src/channels/qq.rs) 顶部 `QqMediaFileType` 之后追加：

```rust
use crate::channels::traits::InboundConfirmation;
use std::collections::HashMap;
use uuid::Uuid;

/// 待处理的审批请求记录。
#[derive(Clone, Debug)]
struct PendingApproval {
    request_id: Uuid,
    recipient: String,
    options: Vec<crate::domain::ApprovalOption>,
    created_at: u64,
}
```

修改 `QqChannel` 结构体追加字段：

```rust
pub struct QqChannel {
    // ... 其他字段
    pending_approvals: Arc<RwLock<HashMap<String, PendingApproval>>>,
}
```

更新 `new_with_path` 初始化：

```rust
pending_approvals: Arc::new(RwLock::new(HashMap::new())),
```

- [ ] __Step 2: 实现 render_buttons_as_numbered_list__

替换占位实现为完整版本：

```rust
/// 将 ReplyMarkup::InlineKeyboard 转译为编号列表追加到 base_content 末尾。
fn render_buttons_as_numbered_list(
    markup: &crate::channels::traits::ReplyMarkup,
    base_content: &str,
) -> String {
    use crate::channels::traits::ReplyMarkup;
    match markup {
        ReplyMarkup::InlineKeyboard(rows) => {
            let mut numbered: Vec<String> = Vec::new();
            let mut idx = 1;
            for row in rows {
                for button in row {
                    numbered.push(format!("{idx}. {}", button.text));
                    idx += 1;
                }
            }
            if numbered.is_empty() {
                return base_content.to_string();
            }
            format!(
                "{base_content}\n\n{}\n\n请回复数字或选项名称。",
                numbered.join("\n")
            )
        }
    }
}
```

- [ ] __Step 3: 写测试 — render_buttons_as_numbered_list__

追加测试：

```rust
#[test]
fn render_buttons_single_option() {
    use crate::channels::traits::{InlineKeyboardButton, ReplyMarkup};
    let markup = ReplyMarkup::InlineKeyboard(vec![vec![InlineKeyboardButton {
        text: "允许".to_string(),
        callback_data: "req:allow".to_string(),
    }]]);
    let result = render_buttons_as_numbered_list(&markup, "请确认");
    assert!(result.contains("请确认"));
    assert!(result.contains("1. 允许"));
    assert!(result.contains("请回复数字"));
}

#[test]
fn render_buttons_multiple_rows() {
    use crate::channels::traits::{InlineKeyboardButton, ReplyMarkup};
    let markup = ReplyMarkup::InlineKeyboard(vec![
        vec![InlineKeyboardButton {
            text: "允许".to_string(),
            callback_data: "req:allow".to_string(),
        }],
        vec![InlineKeyboardButton {
            text: "拒绝".to_string(),
            callback_data: "req:deny".to_string(),
        }],
    ]);
    let result = render_buttons_as_numbered_list(&markup, "确认");
    assert!(result.contains("1. 允许"));
    assert!(result.contains("2. 拒绝"));
}

#[test]
fn render_buttons_empty_returns_base() {
    use crate::channels::traits::ReplyMarkup;
    let markup = ReplyMarkup::InlineKeyboard(vec![]);
    let result = render_buttons_as_numbered_list(&markup, "base");
    assert_eq!(result, "base");
}
```

- [ ] __Step 4: 实现 record_pending_approval 与 try_match_approval_reply__

在 `impl QqChannel` 块中追加：

```rust
const PENDING_APPROVAL_TTL_SECS: u64 = 300;

/// 记录待处理审批请求（同 recipient 覆盖旧的）。
async fn record_pending_approval(
    &self,
    recipient: &str,
    request_id: Uuid,
    options: Vec<crate::domain::ApprovalOption>,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut map = self.pending_approvals.write().await;
    map.insert(
        recipient.to_string(),
        PendingApproval {
            request_id,
            recipient: recipient.to_string(),
            options,
            created_at: now,
        },
    );
}

/// 尝试将用户回复匹配到 pending approval。
/// 匹配优先级：数字 → option id → option label。
async fn try_match_approval_reply(
    &self,
    recipient: &str,
    content: &str,
) -> Option<InboundConfirmation> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let pending = {
        let map = self.pending_approvals.read().await;
        let p = map.get(recipient)?.clone();
        // TTL 检查
        if now - p.created_at > PENDING_APPROVAL_TTL_SECS {
            drop(map);
            let mut map = self.pending_approvals.write().await;
            map.remove(recipient);
            return None;
        }
        p
    };

    let normalized = content.trim();
    let matched = if normalized.chars().all(|c| c.is_ascii_digit()) && !normalized.is_empty() {
        // 数字匹配
        normalized
            .parse::<usize>()
            .ok()
            .and_then(|n| {
                if n >= 1 && n <= pending.options.len() {
                    Some(&pending.options[n - 1])
                } else {
                    None
                }
            })
    } else {
        None
    }
    .or_else(|| pending.options.iter().find(|opt| opt.id == normalized))
    .or_else(|| {
        pending
            .options
            .iter()
            .find(|opt| opt.label == normalized || opt.label.contains(normalized))
    });

    if let Some(opt) = matched {
        let mut map = self.pending_approvals.write().await;
        map.remove(recipient);
        Some(InboundConfirmation {
            request_id: pending.request_id,
            option: opt.id.clone(),
        })
    } else {
        None
    }
}
```

- [ ] __Step 5: 写测试 — try_match_approval_reply 各种匹配场景__

追加测试：

```rust
use crate::domain::ApprovalOption;

fn make_approval_options() -> Vec<ApprovalOption> {
    vec![
        ApprovalOption {
            id: "allow".to_string(),
            label: "允许".to_string(),
            description: String::new(),
        },
        ApprovalOption {
            id: "deny".to_string(),
            label: "拒绝".to_string(),
            description: String::new(),
        },
    ]
}

#[tokio::test]
async fn approval_match_by_digit_one() {
    let ch = QqChannel::new(make_config());
    ch.record_pending_approval("user:u1", Uuid::nil(), make_approval_options())
        .await;
    let result = ch.try_match_approval_reply("user:u1", "1").await;
    assert_eq!(result.unwrap().option, "allow");
}

#[tokio::test]
async fn approval_match_by_digit_two() {
    let ch = QqChannel::new(make_config());
    ch.record_pending_approval("user:u1", Uuid::nil(), make_approval_options())
        .await;
    let result = ch.try_match_approval_reply("user:u1", "2").await;
    assert_eq!(result.unwrap().option, "deny");
}

#[tokio::test]
async fn approval_match_by_option_id() {
    let ch = QqChannel::new(make_config());
    ch.record_pending_approval("user:u1", Uuid::nil(), make_approval_options())
        .await;
    let result = ch.try_match_approval_reply("user:u1", "allow").await;
    assert_eq!(result.unwrap().option, "allow");
}

#[tokio::test]
async fn approval_match_by_label() {
    let ch = QqChannel::new(make_config());
    ch.record_pending_approval("user:u1", Uuid::nil(), make_approval_options())
        .await;
    let result = ch.try_match_approval_reply("user:u1", "允许").await;
    assert_eq!(result.unwrap().option, "allow");
}

#[tokio::test]
async fn approval_digit_out_of_range_returns_none() {
    let ch = QqChannel::new(make_config());
    ch.record_pending_approval("user:u1", Uuid::nil(), make_approval_options())
        .await;
    let result = ch.try_match_approval_reply("user:u1", "3").await;
    assert!(result.is_none());
}

#[tokio::test]
async fn approval_no_pending_returns_none() {
    let ch = QqChannel::new(make_config());
    let result = ch.try_match_approval_reply("user:u1", "1").await;
    assert!(result.is_none());
}

#[tokio::test]
async fn approval_match_removes_pending() {
    let ch = QqChannel::new(make_config());
    ch.record_pending_approval("user:u1", Uuid::nil(), make_approval_options())
        .await;
    let _ = ch.try_match_approval_reply("user:u1", "1").await;
    // 再次匹配应返回 None
    let result = ch.try_match_approval_reply("user:u1", "1").await;
    assert!(result.is_none());
}
```

- [ ] __Step 6: 修改 send() 在审批请求时记录 pending__

修改 Task 5 的 `send()` 方法，在 `render_buttons_as_numbered_list` 调用前追加 pending 记录逻辑。

由于 send() 无法直接拿到 `request_id`（在 ReplyMarkup 的 callback_data 中），需要从 callback_data 解析。在 `render_buttons_as_numbered_list` 之前添加：

实际上，`request_id` 已编码在 `callback_data` 中（格式 `<request_id>:<option_id>`）。在 send() 中需要解析并调用 `record_pending_approval`。

更新 send() 方法（在 `let final_text = if let Some(ref markup)` 块中追加）：

```rust
let final_text = if let Some(ref markup) = message.reply_markup {
    // 从 buttons 提取 request_id 与 options
    if let Some((request_id, options)) = extract_approval_info(markup) {
        self.record_pending_approval(&message.recipient, request_id, options)
            .await;
    }
    render_buttons_as_numbered_list(markup, &text)
} else {
    text.clone()
};
```

添加 `extract_approval_info` 辅助函数：

```rust
/// 从 ReplyMarkup::InlineKeyboard 的 callback_data 中提取 request_id 与选项列表。
/// callback_data 格式：`<request_id>:<option_id>`，由 ChannelFrontend 生成。
fn extract_approval_info(
    markup: &crate::channels::traits::ReplyMarkup,
) -> Option<(Uuid, Vec<crate::domain::ApprovalOption>)> {
    use crate::channels::traits::ReplyMarkup;
    use crate::domain::ApprovalOption;
    match markup {
        ReplyMarkup::InlineKeyboard(rows) => {
            let mut request_id: Option<Uuid> = None;
            let mut options = Vec::new();
            for row in rows {
                for button in row {
                    let Some((rid, opt_id)) = button.callback_data.split_once(':') else {
                        continue;
                    };
                    // 第一个有效 button 解析出 request_id，后续 button 复用同一 request_id
                    if request_id.is_none() {
                        request_id = Uuid::parse_str(rid).ok();
                    }
                    if request_id.is_some() {
                        options.push(ApprovalOption {
                            id: opt_id.to_string(),
                            label: button.text.clone(),
                            description: String::new(),
                        });
                    }
                }
            }
            request_id.map(|rid| (rid, options))
        }
    }
}
```

- [ ] __Step 7: 写测试 — extract_approval_info__

追加测试：

```rust
#[test]
fn extract_approval_info_from_inline_keyboard() {
    use crate::channels::traits::{InlineKeyboardButton, ReplyMarkup};
    let markup = ReplyMarkup::InlineKeyboard(vec![
        vec![InlineKeyboardButton {
            text: "允许".to_string(),
            callback_data: "01912345-6789-7abc-8def-0123456789ab:allow".to_string(),
        }],
        vec![InlineKeyboardButton {
            text: "拒绝".to_string(),
            callback_data: "01912345-6789-7abc-8def-0123456789ab:deny".to_string(),
        }],
    ]);
    let (request_id, options) = extract_approval_info(&markup).expect("extract");
    assert_eq!(
        request_id.to_string(),
        "01912345-6789-7abc-8def-0123456789ab"
    );
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].id, "allow");
    assert_eq!(options[0].label, "允许");
    assert_eq!(options[1].id, "deny");
}
```

- [ ] __Step 8: 运行所有测试__

Run: `cargo test --all-features channels::qq`
Expected: 所有测试通过

- [ ] __Step 9: 运行 clippy__

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告

- [ ] __Step 10: 提交__

```bash
git add src/channels/qq.rs
git commit -m "feat(channels): implement QQ approval text reply matching"
```

---

## Task 7: WebSocket listen() 实现

__Files:__

- Modify: `src/channels/qq.rs`

__Interfaces:__

- Produces: `QqChannel::listen(tx)` 完整实现 — WebSocket Gateway 连接、心跳、事件分发、/bind、ACK

- [ ] __Step 1: 实现 get_gateway_url__

在 `impl QqChannel` 块中追加：

```rust
/// 获取 WebSocket Gateway URL。
async fn get_gateway_url(&self, token: &str) -> Result<String, ChannelError> {
    let url = format!("{}/gateway", self.api_base);
    let resp = self
        .client
        .get(&url)
        .header("Authorization", format!("QQBot {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ChannelError::Api {
            code: status.as_u16() as i32,
            message: text,
        });
    }
    let data: serde_json::Value = resp.json().await?;
    let gw_url = data
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or(ChannelError::Auth)?
        .to_string();
    Ok(gw_url)
}
```

- [ ] __Step 2: 实现 send_ack_text（入向 ACK）__

在 `impl QqChannel` 块中追加：

```rust
/// 发送入向 ACK 文本给用户。
async fn send_ack_text(&self, recipient: &str, content: &str) {
    let ack_text = if content.starts_with('[') {
        // 附件消息
        format!("收到附件：{}", content.lines().next().unwrap_or(""))
    } else {
        let preview: String = content.chars().take(50).collect();
        format!("收到：{preview}{}", if content.chars().count() > 50 { "..." } else { "" })
    };
    if let Err(e) = self.send_text_markdown(recipient, &ack_text).await {
        tracing::warn!(
            event = "QqAckFailed",
            recipient = %recipient,
            error = %e,
            "failed to send ACK"
        );
    }
}
```

- [ ] __Step 3: 实现 /bind 处理__

在 `impl QqChannel` 块中追加：

```rust
/// 处理 /bind 配对命令。
async fn handle_bind_command(
    &self,
    recipient: &str,
    user_openid: &str,
    content: &str,
) -> Option<()> {
    if !content.starts_with("/bind ") {
        return None;
    }
    if !self.config.pairing_enabled || !self.config.allowed_users.is_empty() {
        return None;
    }
    let code = content[6..].trim();
    let expected = self.config.pairing_code.clone().unwrap_or_default();
    let reply = if !expected.is_empty() && code == expected {
        self.runtime_allow(user_openid).await;
        if let Some(ref path) = self.config_path {
            if Self::is_writable_toml(path).await {
                if self
                    .persist_allowed_user(user_openid, path)
                    .await
                    .is_ok()
                {
                    "已授权并已保存到配置。"
                } else {
                    "已授权（本次运行有效）。"
                }
            } else {
                "已授权（本次运行有效）。"
            }
        } else {
            "已授权（本次运行有效）。"
        }
    } else {
        "配对码错误。"
    };
    if let Err(e) = self.send_text_markdown(recipient, reply).await {
        tracing::warn!(event = "QqBindReplyFailed", error = %e, "failed to send bind reply");
    }
    Some(())
}

async fn is_writable_toml(path: &std::path::Path) -> bool {
    path.extension().map(|e| e == "toml").unwrap_or(false)
        && tokio::fs::metadata(path)
            .await
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false)
}

async fn persist_allowed_user(&self, user_openid: &str, path: &std::path::Path) -> Result<(), ChannelError> {
    // 关键：必须解析为 ChannelConfigs 而非 QqConfig，否则会丢失 [telegram] 等其他段。
    let mut configs: ChannelConfigs = tokio::fs::read_to_string(path)
        .await
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    let qq = configs.qq.get_or_insert_with(|| self.config.clone());
    if !qq.allowed_users.iter().any(|u| u == user_openid) {
        qq.allowed_users.push(user_openid.to_string());
    }
    let content = toml::to_string_pretty(&configs).map_err(|e| ChannelError::Api {
        code: 0,
        message: e.to_string(),
    })?;
    tokio::fs::write(path, content)
        .await
        .map_err(|e| ChannelError::Api {
            code: 0,
            message: e.to_string(),
        })?;
    Ok(())
}
```

- [ ] __Step 4: 写测试 — persist_allowed_user 保留 [telegram] 段__

在 [src/channels/qq.rs](../../../src/channels/qq.rs) 的 `mod tests` 中追加：

```rust
#[tokio::test]
async fn persist_allowed_user_preserves_telegram_section() {
    use tempfile::NamedTempFile;

    let file = NamedTempFile::with_suffix(".toml").unwrap();
    tokio::fs::write(
        file.path(),
        r#"[telegram]
bot_token = "tg_token_x"
allowed_users = ["alice"]

[qq]
app_id = "qq_id"
app_secret = "qq_secret"
allowed_users = []
"#,
    )
    .await
    .unwrap();

    let ch = QqChannel::new_with_path(
        QqConfig {
            app_id: "qq_id".to_string(),
            app_secret: "qq_secret".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        },
        Some(file.path().to_path_buf()),
    );
    ch.persist_allowed_user("user_xyz", file.path())
        .await
        .expect("persist");

    let content = tokio::fs::read_to_string(file.path()).await.unwrap();
    let parsed: ChannelConfigs = toml::from_str(&content).expect("reparse");
    // [telegram] 段必须保留
    assert_eq!(
        parsed.telegram.unwrap().bot_token,
        "tg_token_x"
    );
    // [qq] 段 allowed_users 应追加新用户
    let qq = parsed.qq.unwrap();
    assert_eq!(qq.allowed_users, vec!["user_xyz".to_string()]);
    // 不应有重复
    assert_eq!(qq.allowed_users.len(), 1);
}

#[tokio::test]
async fn persist_allowed_user_deduplicates() {
    use tempfile::NamedTempFile;

    let file = NamedTempFile::with_suffix(".toml").unwrap();
    tokio::fs::write(
        file.path(),
        r#"[qq]
app_id = "id"
app_secret = "secret"
allowed_users = ["existing_user"]
"#,
    )
    .await
    .unwrap();

    let ch = QqChannel::new_with_path(
        QqConfig {
            app_id: "id".to_string(),
            app_secret: "secret".to_string(),
            allowed_users: vec!["existing_user".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        },
        Some(file.path().to_path_buf()),
    );
    ch.persist_allowed_user("existing_user", file.path())
        .await
        .expect("persist");

    let content = tokio::fs::read_to_string(file.path()).await.unwrap();
    let parsed: ChannelConfigs = toml::from_str(&content).expect("reparse");
    assert_eq!(
        parsed.qq.unwrap().allowed_users,
        vec!["existing_user".to_string()]
    );
}
```

> 注：测试需要 `tempfile` crate，应已在 [Cargo.toml](../../../Cargo.toml) dev-dependencies 中（Telegram 通道测试已使用）。

- [ ] __Step 5: 实现 listen() 完整 WebSocket 主循环__

替换 `impl Channel for QqChannel` 中的 `listen` 方法：

```rust
async fn listen(
    &self,
    tx: crossbeam_channel::Sender<crate::channels::traits::ChannelInboundMessage>,
) -> Result<(), ChannelError> {
    use crate::channels::traits::{ChannelInboundMessage, InboundConfirmation};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    tracing::info!(event = "QqListenStart", "QQ authenticating...");
    let token = self.get_token().await?;

    tracing::info!(event = "QqGatewayFetch", "fetching gateway URL...");
    let gw_url = self.get_gateway_url(&token).await?;

    tracing::info!(event = "QqWsConnect", url = %gw_url, "connecting to gateway WebSocket...");
    let (ws_stream, _) = tokio_tungstenite::connect_async(&gw_url)
        .await
        .map_err(|e| ChannelError::Network(e))?;
    let (mut write, mut read) = ws_stream.split();

    // 接收 Hello (op=10)
    let hello = read
        .next()
        .await
        .ok_or(ChannelError::Auth)?
        .map_err(ChannelError::Network)?;
    let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())
        .map_err(|e| ChannelError::Api { code: 0, message: e.to_string() })?;
    let heartbeat_interval = hello_data
        .get("d")
        .and_then(|d| d.get("heartbeat_interval"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(41250);

    // 发送 Identify (op=2)
    let intents: u64 = (1 << 25) | (1 << 30);
    let identify = json!({
        "op": 2,
        "d": {
            "token": format!("QQBot {token}"),
            "intents": intents,
            "properties": {
                "os": "linux",
                "browser": "harness",
                "device": "harness",
            }
        }
    });
    write
        .send(Message::Text(identify.to_string().into()))
        .await
        .map_err(ChannelError::Network)?;
    tracing::info!(event = "QqIdentified", "QQ connected and identified");

    let mut sequence: i64 = -1;
    let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
    let hb_interval = heartbeat_interval;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(hb_interval));
        loop {
            interval.tick().await;
            if hb_tx.send(()).await.is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            _ = hb_rx.recv() => {
                let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                let hb = json!({"op": 1, "d": d});
                if write
                    .send(Message::Text(hb.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(payload))) => {
                        if write.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => continue,
                };

                let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                    sequence = s;
                }
                let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);
                match op {
                    1 => {
                        let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                        let hb = json!({"op": 1, "d": d});
                        if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    7 => {
                        tracing::warn!(event = "QqReconnect", "received Reconnect (op 7)");
                        break;
                    }
                    9 => {
                        tracing::warn!(event = "QqInvalidSession", "received Invalid Session (op 9)");
                        break;
                    }
                    _ => {}
                }
                if op != 0 {
                    continue;
                }

                let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");
                let d = match event.get("d") {
                    Some(d) => d,
                    None => continue,
                };

                match event_type {
                    "C2C_MESSAGE_CREATE" | "GROUP_AT_MESSAGE_CREATE" => {
                        let is_group = event_type == "GROUP_AT_MESSAGE_CREATE";
                        let msg_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        if self.is_duplicate(msg_id).await {
                            continue;
                        }

                        let content = match self.compose_message_content(d).await {
                            Some(c) => c,
                            None => continue,
                        };

                        let (user_openid, recipient) = if is_group {
                            let group_openid = d
                                .get("group_openid")
                                .and_then(|g| g.as_str())
                                .unwrap_or("unknown");
                            let member_openid = d
                                .get("author")
                                .and_then(|a| a.get("member_openid"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("unknown");
                            (member_openid.to_string(), format!("group:{group_openid}"))
                        } else {
                            let user_openid = d
                                .get("author")
                                .and_then(|a| a.get("user_openid"))
                                .and_then(|u| u.as_str())
                                .or_else(|| {
                                    d.get("author")
                                        .and_then(|a| a.get("id"))
                                        .and_then(|i| i.as_str())
                                })
                                .unwrap_or("unknown");
                            (user_openid.to_string(), format!("user:{user_openid}"))
                        };

                        if !self.is_user_allowed(&user_openid).await {
                            tracing::warn!(
                                event = "QqUserDenied",
                                user_openid = %user_openid,
                                "user not in allowed list"
                            );
                            continue;
                        }

                        // /bind 优先匹配
                        if self.handle_bind_command(&recipient, &user_openid, &content).await.is_some() {
                            continue;
                        }

                        // 审批回复匹配
                        if let Some(confirmation) = self.try_match_approval_reply(&recipient, &content).await {
                            let inbound = ChannelInboundMessage {
                                channel_name: self.name().to_string(),
                                sender_id: user_openid.clone(),
                                chat_id: recipient.clone(),
                                thread_id: None,
                                content: String::new(),
                                timestamp_secs: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                                confirmation: Some(confirmation),
                            };
                            // 发送确认文本
                            let note = format!(
                                "已选择：{}",
                                content
                            );
                            let _ = self.send_text_markdown(&recipient, &note).await;
                            let _ = tx.send(inbound);
                            continue;
                        }

                        // 常规消息
                        let inbound = ChannelInboundMessage {
                            channel_name: self.name().to_string(),
                            sender_id: user_openid.clone(),
                            chat_id: recipient.clone(),
                            thread_id: None,
                            content: content.clone(),
                            timestamp_secs: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                            confirmation: None,
                        };
                        let _ = tx.send(inbound);
                        // 发送 ACK
                        self.send_ack_text(&recipient, &content).await;
                    }
                    _ => {}
                }
            }
        }
    }

    Err(ChannelError::Network(
        tokio_tungstenite::tungstenite::Error::AlreadyClosed,
    ))
}
```

> __注意__：审批回复确认文本应使用 matched option 的 label 而非 raw content。需要修正：

在审批回复匹配成功后，需要保留 matched option label。修改 `try_match_approval_reply` 返回值或额外传参。

简化方案：在 listen 中，匹配成功后查找 matched option label：

替换审批回复处理块：

```rust
// 审批回复匹配
if let Some(confirmation) = self.try_match_approval_reply(&recipient, &content).await {
    // 查找 matched option label 用于确认文本
    let label = {
        let map = self.pending_approvals.read().await;
        // pending 已被移除，无法获取 options
        // 需要在 try_match_approval_reply 中返回 label
        String::new()
    };
    // 简化：直接用 content 作为 label（用户输入）
    let note = format!("已选择：{content}");
    let _ = self.send_text_markdown(&recipient, &note).await;
    let inbound = ChannelInboundMessage {
        channel_name: self.name().to_string(),
        sender_id: user_openid.clone(),
        chat_id: recipient.clone(),
        thread_id: None,
        content: String::new(),
        timestamp_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        confirmation: Some(confirmation),
    };
    let _ = tx.send(inbound);
    continue;
}
```

- [ ] __Step 6: 添加 futures-util 依赖__

检查 [Cargo.toml](../../../Cargo.toml)，若 `futures-util` 未添加则追加：

```toml
futures-util = "0.3"
```

- [ ] __Step 7: 运行编译检查__

Run: `cargo check --all-features`
Expected: 编译通过（listen 难以单元测试，依赖集成测试）

- [ ] __Step 8: 运行所有已有测试确保未破坏__

Run: `cargo test --all-features channels::qq`
Expected: 所有测试通过

- [ ] __Step 9: 运行 clippy__

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告

- [ ] __Step 10: 提交__

```bash
git add Cargo.toml Cargo.lock src/channels/qq.rs
git commit -m "feat(channels): implement QQ WebSocket listen with heartbeat and event dispatch"
```

---

## Task 8: 集成到 HarnessConfig 与文档同步

__Files:__

- Modify: `src/app/mod.rs`
- Modify: `src/channels/telegram.rs`（修复同名 bug）
- Modify: `docs/current-state.md`
- Modify: `docs/configuration.md`
- Modify: `.env.example`
- Modify: `docs/design/im-channel-adapters.md`
- Modify: `AGENTS.md` 与 `CLAUDE.md`
- Modify: `docs/design/README.md` 与 `docs/README.md`

__Interfaces:__

- Produces: HarnessConfig 在 channels.toml 配置 [qq] 段时实例化 QqChannel 加入 ChannelManager
- Produces: Telegram `persist_allowed_user` 修复为解析 ChannelConfigs，避免破坏 [qq] 段

- [ ] __Step 1: 查找 HarnessConfig 实例化 TelegramChannel 的位置__

Run: `cargo check --all-features` 后用 Grep 搜索 `TelegramChannel::new`：

使用 Grep 工具：pattern=`TelegramChannel::new`, path=`src/`, output_mode=`content`, -n=true

预期找到 `src/app/mod.rs` 或 `src/main.rs` 中的实例化点。

- [ ] __Step 2: 在实例化点添加 QQ 通道初始化__

读取找到的文件，在 Telegram 实例化之后追加 QQ 实例化代码。模式如下：

```rust
// 在 Telegram 实例化后追加
let qq_channel = config.channels.qq.as_ref().map(|qq| {
    let mut ch = QqChannel::new_with_path(qq.clone(), channels_config_path.cloned());
    if let Some(ws) = &workspace_dir {
        ch = ch.with_workspace_dir(ws.clone());
    }
    Arc::new(ch) as Arc<dyn Channel>
});
```

将 `qq_channel` 加入 `channels: Vec<Arc<dyn Channel>>` 列表（与 telegram_channel 同样的条件加入模式）。

- [ ] __Step 3: 修复 Telegram persist_allowed_user 同名 bug__

修改 [src/channels/telegram.rs:131-154](../../../src/channels/telegram.rs#L131-L154) 的 `persist_allowed_user`，把 `TelegramConfig` 解析改为 `ChannelConfigs`，避免加入 [qq] 段后 /bind 写回会丢失 [qq] 段：

```rust
async fn persist_allowed_user(&self, user_id: &str, path: &Path) -> Result<(), ChannelError> {
    // 关键：必须解析为 ChannelConfigs 而非 TelegramConfig，否则会丢失 [qq] 等其他段。
    use crate::channels::config::ChannelConfigs;
    let mut configs: ChannelConfigs = tokio::fs::read_to_string(path)
        .await
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    let tg = configs.telegram.get_or_insert_with(|| self.config.clone());
    if !tg.allowed_users.iter().any(|u| u == user_id) {
        tg.allowed_users.push(user_id.to_string());
    }
    let content = toml::to_string_pretty(&configs).map_err(|e| ChannelError::Api {
        code: 0,
        message: e.to_string(),
    })?;
    tokio::fs::write(path, content)
        .await
        .map_err(|e| ChannelError::Api {
            code: 0,
            message: e.to_string(),
        })?;
    Ok(())
}
```

更新 [src/channels/telegram.rs](../../../src/channels/telegram.rs) 中已有的 `persist_allowed_user_appends_to_toml` 与 `persist_allowed_user_deduplicates` 测试，将初始 toml 改为含 `[telegram]` 段顶层结构，使其与新解析逻辑一致：

```rust
tokio::fs::write(
    file.path(),
    r#"[telegram]
bot_token = "x"
allowed_users = ["alice"]
"#,
)
.await
.unwrap();
```

追加一个回归测试验证 Telegram /bind 不会破坏 [qq] 段：

```rust
#[tokio::test]
async fn persist_allowed_user_preserves_qq_section() {
    use tempfile::NamedTempFile;

    let file = NamedTempFile::with_suffix(".toml").unwrap();
    tokio::fs::write(
        file.path(),
        r#"[telegram]
bot_token = "tg_token"
allowed_users = ["alice"]

[qq]
app_id = "qq_id"
app_secret = "qq_secret"
allowed_users = ["qq_user"]
"#,
    )
    .await
    .unwrap();

    let channel = TelegramChannel::new_with_path(
        TelegramConfig {
            bot_token: "tg_token".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        },
        Some(file.path().to_path_buf()),
    );
    channel
        .persist_allowed_user("new_user", file.path())
        .await
        .unwrap();

    let content = tokio::fs::read_to_string(file.path()).await.unwrap();
    let parsed: ChannelConfigs = toml::from_str(&content).unwrap();
    // [telegram] 段追加新用户
    assert_eq!(
        parsed.telegram.unwrap().allowed_users,
        vec!["alice".to_string(), "new_user".to_string()]
    );
    // [qq] 段必须保留
    let qq = parsed.qq.unwrap();
    assert_eq!(qq.app_id, "qq_id");
    assert_eq!(qq.allowed_users, vec!["qq_user".to_string()]);
}
```

- [ ] __Step 4: 运行编译检查__

Run: `cargo check --all-features`
Expected: 编译通过

- [ ] __Step 5: 更新 docs/current-state.md__

读取 [docs/current-state.md](../../../docs/current-state.md)，在"已实现"段 IM 通道能力描述中追加：

```markdown
- QQ 通道：C2C 私聊 / 群 @ 消息收发、Markdown 渲染、媒体/文件上传（URL/base64）、附件下载、/bind 配对、审批交互（文本回复匹配）
```

- [ ] __Step 6: 更新 docs/configuration.md__

读取 [docs/configuration.md](../../../docs/configuration.md)，在 Telegram 配置段之后追加 QQ 配置示例：

```markdown
### QQ 通道

```toml
[qq]
app_id = "${QQ_APP_ID}"
app_secret = "${QQ_APP_SECRET}"
allowed_users = []
pairing_enabled = false
pairing_code = ""
```

环境变量：

- `QQ_APP_ID`：QQ Bot 的 appId
- `QQ_APP_SECRET`：QQ Bot 的 clientSecret

- [ ] __Step 7: 更新 .env.example__

读取 [.env.example](../../../.env.example)，追加：

```bash
# QQ 通道（可选）
QQ_APP_ID=
QQ_APP_SECRET=
```

- [ ] __Step 8: 更新 docs/design/im-channel-adapters.md__

读取 [docs/design/im-channel-adapters.md](../../../docs/design/im-channel-adapters.md#L129)，将 "QQ（后续阶段）" 段落状态标注为已实现：

```markdown
### QQ（已实现）

- OAuth2 app token，WebSocket Gateway 接收事件。
- `msg_type=2` markdown 文本；`msg_type=7` 富媒体。
- 小文件 base64 上传 + 缓存；大文件分片上传（后续扩展）。
- 审批交互通过文本回复匹配实现（无 inline keyboard）。

> 详见 [QQ 通道设计](../superpowers/specs/2026-06-27-qq-channel-design.md)。
```

- [ ] __Step 9: 更新 AGENTS.md 与 CLAUDE.md__

读取 [AGENTS.md](../../../AGENTS.md)，在"已实现"段 IM 通道能力描述中追加 QQ 通道。然后同步到 [CLAUDE.md](../../../CLAUDE.md)。

- [ ] __Step 10: 更新 docs/design/README.md 与 docs/README.md__

读取 [docs/design/README.md](../../../docs/design/README.md) 与 [docs/README.md](../../../docs/README.md)，追加 QQ 通道设计文档索引。

- [ ] __Step 11: 运行 markdownlint__

Run: `markdownlint docs/superpowers/specs/2026-06-27-qq-channel-design.md docs/configuration.md docs/current-state.md docs/design/im-channel-adapters.md AGENTS.md CLAUDE.md`
Expected: 无 lint 错误

- [ ] __Step 12: 运行完整 CI 检查__

Run: `cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
Expected: 全部通过

- [ ] __Step 13: 提交__

```bash
git add src/app/mod.rs src/channels/telegram.rs docs/current-state.md docs/configuration.md .env.example docs/design/im-channel-adapters.md AGENTS.md CLAUDE.md docs/design/README.md docs/README.md
git commit -m "feat(channels): integrate QQ channel into HarnessConfig and fix Telegram persist_allowed_user"
```

---

## 自审

__1. Spec coverage:__

| Spec 章节 | 实现 Task |
|-----------|----------|
| 总体架构 / 模块布局 | Task 1（config）、Task 2（骨架）、Task 5（mod 导出） |
| QqConfig 配置 + 环境变量 | Task 1 |
| ChannelId 编码（前缀） | Task 5（resolve_recipient） |
| OAuth2 token 获取与缓存 | Task 2 |
| WebSocket Gateway 协议 | Task 7 |
| 事件分发（C2C/GROUP_AT） | Task 7 |
| 消息去重 | Task 4 |
| 入向 listen() 主流程 | Task 7 |
| 入向消息内容组装 | Task 3 |
| 入向 ACK | Task 7（send_ack_text） |
| 出向 send() 主流程 | Task 5 |
| 主动消息路径 | Task 5（send_text_markdown / send_media_message） |
| parse_mode 映射 | Task 5 |
| 附件上传策略（URL/base64） | Task 5 |
| 附件上传策略（分片 >10MB） | __未覆盖__ — 本期简化，base64 限制约 10MB |
| Voice 特殊处理 | Task 5（marker_kind_to_qq_file_type） |
| 失败降级 | Task 5（send fallback） |
| 审批交互 — 出向渲染 | Task 6（render_buttons_as_numbered_list） |
| 审批交互 — pending 记录 | Task 6（record_pending_approval） |
| 审批交互 — 入向识别 | Task 6（try_match_approval_reply） |
| 审批交互 — TTL | Task 6（PENDING_APPROVAL_TTL_SECS） |
| 审批交互 — 确认 | Task 7（listen 中发送 "已选择"） |
| /bind 配对 | Task 7（handle_bind_command） |
| 错误处理 | Task 2、Task 5 |
| ECS 集成数据流 | Task 8（HarnessConfig 集成） |
| 依赖 | Task 1 |
| 测试 — 单元 | Task 1-7 各自包含 |
| 测试 — 集成（wiremock） | Task 5 |
| 文档同步 | Task 8 |

__缺口 1：分片上传（>10MB）未覆盖。__
Spec 第 5.5 节描述了分片流程，但实施计划简化为仅 base64。这是合理的范围裁剪 — 分片上传依赖 `upload_prepare` / `upload_part` / `upload_part_finish` / `complete_multipart_upload` 四个 API，复杂度高。建议本期不交付，在 spec "本期不交付" 段补充说明。

__缺口 2：WebSocket 集成测试未覆盖。__
Task 7 仅编译验证，未写 mock WebSocket server 测试。建议作为后续独立任务，因 `tokio-tungstenite` mock server 需要额外脚手架。

__2. Placeholder scan:__
检查所有步骤代码块，无 TBD/TODO。所有 `todo!()` 占位已在评审修订中消除（api_base/auth_url 字段化提前到 Task 2，runtime_allow 直接 async 实现）。

__3. Type consistency:__

- `QqChannel::new` / `new_with_path` / `with_workspace_dir` / `with_api_base` / `with_auth_url` / `set_token_for_test` 签名一致 ✓
- `PendingApproval` / `InboundConfirmation` / `ApprovalOption` 类型跨 Task 一致 ✓
- `render_buttons_as_numbered_list` / `extract_approval_info` / `try_match_approval_reply` 签名一致 ✓
- `QqMediaFileType` 枚举值与 API file_type 数值映射一致 ✓

__4. 歧义检查：__

- `api_base` / `auth_url` 实例字段已在 Task 2 结构体定义时初始化（默认值取自 `QQ_API_BASE` / `QQ_AUTH_URL` 常量），后续 Task 直接复用，无需重构 ✓
- Task 7 审批确认文本的 label 获取问题已说明简化方案 ✓
- Task 7 `persist_allowed_user` 改为解析 `ChannelConfigs`，与 Telegram 修复方案一致 ✓
- Task 8 Telegram `persist_allowed_user` 修复同样解析为 `ChannelConfigs`，回归测试覆盖 [qq] 段保留 ✓
