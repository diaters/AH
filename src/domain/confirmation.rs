//! 确认与审批相关类型定义
//!
//! 定义用户确认、父 Agent 审批等类型。

use serde::{Deserialize, Serialize};

/// 授权模式（用户确认和父 Agent 审批共用）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantMode {
    /// 单次授权，仅本次执行
    Once,
    /// 永久授权，更新 Agent 权限配置
    Permanent,
}

/// 审批来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConfirmationSource {
    #[default]
    User,
    ParentAgent,
}

/// 确认选项
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmationOption {
    /// 选项标识
    pub id: String,
    /// 显示文本
    pub label: String,
    /// 确认模式
    pub mode: GrantMode,
}

impl ConfirmationOption {
    /// 创建 "允许一次" 选项
    pub fn allow_once() -> Self {
        Self {
            id: "allow_once".to_string(),
            label: "Allow once".to_string(),
            mode: GrantMode::Once,
        }
    }

    /// 创建 "永久允许" 选项
    pub fn allow_always() -> Self {
        Self {
            id: "allow_always".to_string(),
            label: "Allow always".to_string(),
            mode: GrantMode::Permanent,
        }
    }

    /// 创建 "拒绝" 选项
    pub fn deny() -> Self {
        Self {
            id: "deny".to_string(),
            label: "Deny".to_string(),
            mode: GrantMode::Once, // Deny 模式不影响 Permanent
        }
    }

    /// 判断是否为拒绝选项
    pub fn is_deny(&self) -> bool {
        self.id == "deny"
    }

    /// 获取默认选项列表
    pub fn default_options() -> Vec<Self> {
        vec![Self::allow_once(), Self::allow_always(), Self::deny()]
    }
}

/// 审批决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}
