//! 插件 Slash Command 基础设施
//!
//! 插件贡献的 slash command 识别与派发逻辑。
//! 解析逻辑位于 `src/domain/command.rs` 的 `UserCommand::PluginCommand`，
//! 派发逻辑位于 `src/systems/command.rs` 的 `command_parse_system`。
//!
//! 完整的 Rhai 脚本派发将在 Phase 8 集成测试时实现。
