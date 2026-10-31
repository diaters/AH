//! Harness 运行时配置
//!
//! `HarnessConfig` 聚合运行时各域配置（模型、通道、超时与路径），
//! 经 `HarnessSettings` Resource 注入 World，供各 system 读取。
//! 归属 systems 层：消费主体是运行时系统，装配由 app/main 完成。

use anyhow::{Context, Result};

use crate::domain::{LoopControlConfig, MemoryConfig};
use crate::llm::{LlmProviderConfig, LlmProviderKind};
use crate::prelude::Resource;

/// 主 Agent 的默认名称：用户任务直接交给它处理。
pub const DEFAULT_MAIN_AGENT: &str = "default-llm-agent";

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub max_retries: u32,
    pub max_tool_iterations: u32,
    pub llm: LlmProviderConfig,
    /// 主 Agent 名称（`HARNESS_MAIN_AGENT`）：用户任务与未指定 Agent 的子任务直接交给它。
    pub main_agent: String,
    /// 记忆压缩配置（token 阈值 / 摘要目标）
    pub memory: MemoryConfig,
    /// Agent loop 控制配置（自动拉起预算 / 通知合并 / 后台轮询）
    pub loop_control: LoopControlConfig,
    pub agents_config_path: String,
    /// wait_tasks 工具的默认超时时间（秒）
    pub default_wait_tasks_timeout_secs: u64,
    /// shell 工具默认返回的最新输出行数
    pub shell_default_tail_lines: usize,
    /// shell 工具允许返回的最大输出行数
    pub shell_max_tail_lines: usize,
    /// shell.exec 默认超时时间（秒）
    pub shell_default_exec_timeout_secs: u64,
    /// shell.stop(wait_for_exit=true) 默认超时时间（秒）
    pub shell_default_stop_timeout_secs: u64,
    /// 异步工具桥的失联超时（秒）—— sweeper 推导 max_duration 的全局缺省。
    ///
    /// 默认 300，与 `default_wait_tasks_timeout_secs` 同量级：
    /// 既覆盖典型 LLM 工具链路（含模型回包 ~ 60s），也兜住 wait_tasks 这种
    /// 长任务场景。具体工具可在 `BuiltinTool::max_duration` override 中
    /// 基于业务超时 + margin 推导。
    pub tool_inflight_timeout_secs: u64,
    /// 每个 session stream 的最大缓存字节数
    pub shell_max_buffer_bytes_per_stream: usize,
    /// 终态任务树保留时长（秒），超过后整树回收，避免长期运行时实体无限增长
    pub terminal_task_retention_secs: u64,
    /// 顶层多轮会话等待用户输入的空闲上限（秒），超过后自动结束；0 表示关闭
    pub conversation_idle_secs: u64,
    /// 连续失败达到该次数时首次告警，默认3。
    pub schedule_failure_alert_threshold: u32,
    /// 已废弃的兼容字段；按需驱动不使用该值。
    pub active_poll_ms: u64,
    /// 已废弃的兼容字段；按需驱动不使用该值。
    pub idle_poll_ms: u64,
    pub channels: crate::channels::config::ChannelConfigs,
    /// IM 通道配置文件路径（用于 Telegram /bind 回写）
    pub channels_config_path: Option<String>,
    /// 触发器配置文件路径（用于 webhook/timer 事件路由）
    pub triggers_config_path: Option<String>,
    /// providers 配置文件路径（用于多 provider 注册）
    pub providers_config_path: String,
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self> {
        for name in ["HARNESS_ACTIVE_POLL_MS", "HARNESS_IDLE_POLL_MS"] {
            if std::env::var_os(name).is_some() {
                tracing::warn!(
                    event = "DeprecatedPollingConfig",
                    variable = name,
                    "polling setting is ignored; runtime now waits for input, results and deadlines"
                );
            }
        }
        let llm = LlmProviderConfig::from_env("gpt-4.1-mini")?;

        let main_agent = std::env::var("HARNESS_MAIN_AGENT")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_MAIN_AGENT.to_string());

        let agents_config_path =
            std::env::var("HARNESS_AGENTS_CONFIG").unwrap_or_else(|_| "agents.toml".to_string());

        Ok(Self {
            schedule_failure_alert_threshold: std::env::var(
                "HARNESS_SCHEDULE_FAILURE_ALERT_THRESHOLD",
            )
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value| *value > 0)
            .unwrap_or(3),
            max_retries: std::env::var("HARNESS_MAX_RETRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            max_tool_iterations: std::env::var("HARNESS_MAX_TOOL_ITERATIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            llm,
            main_agent,
            memory: MemoryConfig {
                compression_threshold_tokens: std::env::var(
                    "HARNESS_MEMORY_COMPRESSION_THRESHOLD_TOKENS",
                )
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8000),
                summary_target_tokens: std::env::var("HARNESS_MEMORY_SUMMARY_TARGET_TOKENS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1000),
            },
            loop_control: {
                let defaults = LoopControlConfig::default();
                LoopControlConfig {
                    max_auto_wakes: std::env::var("HARNESS_MAX_AUTO_WAKES")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(defaults.max_auto_wakes),
                    auto_wake_window_secs: std::env::var("HARNESS_AUTO_WAKE_WINDOW_SECS")
                        .ok()
                        .and_then(|value| value.parse::<u64>().ok())
                        .filter(|seconds| *seconds > 0)
                        .unwrap_or(defaults.auto_wake_window_secs),
                    notification_coalesce_ms: std::env::var("HARNESS_NOTIFICATION_COALESCE_MS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(defaults.notification_coalesce_ms),
                    background_poll_ms: std::env::var("HARNESS_BACKGROUND_POLL_MS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(defaults.background_poll_ms),
                }
            },
            agents_config_path,
            default_wait_tasks_timeout_secs: std::env::var(
                "HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
            shell_default_tail_lines: std::env::var("HARNESS_SHELL_DEFAULT_TAIL_LINES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(200),
            shell_max_tail_lines: std::env::var("HARNESS_SHELL_MAX_TAIL_LINES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500),
            shell_default_exec_timeout_secs: std::env::var(
                "HARNESS_SHELL_DEFAULT_EXEC_TIMEOUT_SECS",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
            shell_default_stop_timeout_secs: std::env::var(
                "HARNESS_SHELL_DEFAULT_STOP_TIMEOUT_SECS",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10),
            tool_inflight_timeout_secs: std::env::var("HARNESS_TOOL_INFLIGHT_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            shell_max_buffer_bytes_per_stream: std::env::var(
                "HARNESS_SHELL_MAX_BUFFER_BYTES_PER_STREAM",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64 * 1024),
            terminal_task_retention_secs: std::env::var("HARNESS_TERMINAL_TASK_RETENTION_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3600),
            conversation_idle_secs: std::env::var("HARNESS_CONVERSATION_IDLE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(86400),
            active_poll_ms: std::env::var("HARNESS_ACTIVE_POLL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(16),
            idle_poll_ms: std::env::var("HARNESS_IDLE_POLL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(150),
            channels: {
                let path = std::env::var("HARNESS_CHANNELS_CONFIG").ok();
                match path {
                    Some(ref p) if !p.is_empty() => {
                        let text = std::fs::read_to_string(p)
                            .with_context(|| format!("read channels config: {p}"))?;
                        let mut cfg: crate::channels::config::ChannelConfigs =
                            toml::from_str(&text).context("parse channels config")?;
                        cfg.expand_env_vars();
                        cfg
                    }
                    _ => crate::channels::config::ChannelConfigs::default(),
                }
            },
            channels_config_path: std::env::var("HARNESS_CHANNELS_CONFIG").ok(),
            triggers_config_path: std::env::var("HARNESS_TRIGGERS_CONFIG").ok(),
            providers_config_path: std::env::var("HARNESS_PROVIDERS_CONFIG")
                .unwrap_or_else(|_| "providers.toml".to_string()),
        })
    }
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            max_tool_iterations: 5,
            llm: LlmProviderConfig {
                provider: LlmProviderKind::OpenAi,
                model: Some("gpt-4.1-mini".to_string()),
                api_key: Some("test-api-key".to_string()),
                api_base: None,
            },
            main_agent: DEFAULT_MAIN_AGENT.to_string(),
            memory: MemoryConfig::default(),
            loop_control: LoopControlConfig::default(),
            agents_config_path: "agents.toml".to_string(),
            default_wait_tasks_timeout_secs: 300, // 5 minutes default
            shell_default_tail_lines: 200,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 300,
            shell_default_stop_timeout_secs: 10,
            tool_inflight_timeout_secs: 300,
            shell_max_buffer_bytes_per_stream: 64 * 1024,
            terminal_task_retention_secs: 3600,
            conversation_idle_secs: 86400,
            schedule_failure_alert_threshold: 3,
            active_poll_ms: 16,
            idle_poll_ms: 150,
            channels: crate::channels::config::ChannelConfigs::default(),
            channels_config_path: None,
            triggers_config_path: None,
            providers_config_path: "providers.toml".to_string(),
        }
    }
}

#[derive(Resource)]
pub struct HarnessSettings(pub HarnessConfig);

impl HarnessSettings {
    /// 测试构造器：基于 `HarnessConfig::default()`（`tool_inflight_timeout_secs=300`，
    /// 其余字段按现有测试惯例填默认值）。
    ///
    /// 与 `OwnedToolContext::empty_for_test` 同风格：保持 `pub fn`（不挂 `#[cfg(test)]`），
    /// 让集成测试可经 `harness::systems::HarnessSettings::default_test()` 直接构造。
    pub fn default_test() -> Self {
        Self(HarnessConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_for_poll_intervals() {
        let config = HarnessConfig::default();
        assert_eq!(config.active_poll_ms, 16);
        assert_eq!(config.idle_poll_ms, 150);
    }
}
