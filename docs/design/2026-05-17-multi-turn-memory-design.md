# Phase 4.1 多轮对话与记忆管理设计 v2

> 本文档基于讨论修订，解决 v1 设计中的问题。

---

## 一、核心设计原则

### 1. 任务主题

- **首次输入**：创建 Task，content 为用户输入原文（待总结状态）
- **主题确定**：Agent 首次响应后触发总结，确定任务主题
- **主题不变**：一旦确定，Task 主题不再变化
- **话题隔离**：无关话题通过子任务承接，避免污染主任务上下文

### 2. 上下文隔离机制

```
主任务：主题 A
    │
    ├── 用户谈论 A → 正常多轮对话
    │
    └── 用户谈论 B（无关话题）
            │
            └── 创建子任务：主题 B
                    │
                    ├── 子任务独立上下文
                    │
                    └── 子任务结束 → 总结注入主任务（少量上下文）
```

### 3. 记忆压缩策略

**触发条件**：
- Token 数超过阈值
- 用户输入特定指令（如 `/summarize`）

**压缩策略**：
- 全量 + 摘要模式
- 保留最近 2 轮对话不压缩
- 旧内容压缩为摘要前缀

---

## 二、Task 状态机修订

### 新增状态

```rust
pub enum TaskStatus {
    Pending,           // 新建，待总结主题
    Ready,             // 就绪，等待执行
    Running,           // 执行中
    Waiting(WaitingReason),
    Done,
    Failed(FailureReason),
}
```

### 状态流转

```
Pending ──[总结完成]──→ Ready ──[开始执行]──→ Running
                           ↑                      │
                           │                      ↓
                           └──[用户继续输入]── Waiting(User)
                                                      │
                                                      ↓
                                               [执行完成]
                                                      │
                                    ┌─────────────────┼─────────────────┐
                                    ↓                 ↓                 ↓
                                 Done          Waiting(User)      Waiting(Evaluator)
                                [结束]        [需要更多信息]        [需要评估]
```

### 首次输入流程

```
用户首次输入
    │
    ↓
创建 Task (status = Pending, content = 用户输入原文)
    │
    ↓
Agent 执行
    │
    ↓
触发总结 → 确定 Task 主题
    │
    ↓
Task 更新 (status = Ready/Running, content = "任务主题")
```

---

## 三、记忆结构修订

### ShortTermMemory

```rust
#[derive(Component, Default)]
pub struct ShortTermMemory {
    /// 完整对话条目
    pub entries: Vec<MemoryEntry>,
    
    /// 摘要前缀（压缩后的旧内容）
    pub summary_prefix: Option<String>,
    
    /// 当前 token 估算
    pub estimated_tokens: u32,
    
    /// 最后一次缓存命中的 token 数
    pub last_cached_tokens: Option<u32>,
}
```

### 移除轮数相关字段

- ~~turn_count~~ → 改用 `entries.len()` 或不追踪
- ~~summary_range~~ → 不再需要，摘要与原文分离

### MemoryEntry 保持不变

```rust
pub struct MemoryEntry {
    pub role: EntryRole,
    pub content: String,
    pub metadata: EntryMetadata,
}
```

---

## 四、Token 触发机制

### 配置

```toml
[memory]
# 压缩触发阈值（token 数）
compression_threshold_tokens = 8000

# 保留最近 N 轮不压缩
preserve_recent_turns = 2

# LLM 摘要目标 token 数
summary_target_tokens = 1000
```

### 压缩流程

```
estimated_tokens > compression_threshold_tokens
    │
    ↓
计算需要压缩的条目（排除最近 2 轮）
    │
    ↓
发送给 LLM 生成摘要
    │
    ↓
摘要替换旧条目，更新 summary_prefix
    │
    ↓
重新估算 token 数
```

### Token 估算

使用简单的字符估算（中文约 1.5 字符/token，英文约 4 字符/token），或调用 tokenizer。

---

## 五、总结触发机制

### 触发条件

| 条件 | 说明 |
|------|------|
| Token 阈值 | `estimated_tokens > compression_threshold_tokens` |
| 用户指令 | 用户输入 `/summarize` 等指令 |
| 任务完成 | 子任务结束，总结注入父任务 |

### 总结流程

```
触发总结
    │
    ↓
收集需要总结的内容
    │
    ├── 新建 Task：总结 pending 状态
    │   └── content = 用户输入原文
    │
    └── 已有 Task：已有上下文
        └── entries + summary_prefix
    │
    ↓
调用 LLM 生成摘要
    │
    ↓
更新 Task.content（首次）
或更新 summary_prefix（后续压缩）
```

---

## 六、上下文构建

### 构建 LLM 上下文

```rust
impl ShortTermMemory {
    pub fn build_context(&self) -> Vec<Message> {
        let mut messages = Vec::new();
        
        // 1. 摘要前缀（如果有）
        if let Some(summary) = &self.summary_prefix {
            messages.push(Message::system(summary));
        }
        
        // 2. 最近 N 轮对话（完整保留）
        for entry in &self.entries {
            messages.extend(entry.to_messages());
        }
        
        messages
    }
}
```

### 缓存友好

- summary_prefix 位于对话开头，稳定
- 新对话追加在末尾
- LLM Provider 可缓存前缀部分

---

## 七、子任务与上下文隔离

### 话题偏离检测

```
用户输入与当前任务主题不相关
    │
    ↓
创建子任务（新主题）
    │
    ├── 父任务 ID 记录
    ├── 独立的 ShortTermMemory
    └── 初始上下文：仅用户当前输入
    │
    ↓
子任务独立执行
    │
    ↓
子任务结束 → 总结注入父任务 LongTermMemory
```

### 判断话题偏离

方式待定：
- A. LLM 判断（成本高但准确）
- B. 关键词匹配（简单但不准确）
- C. 用户显式指定（最可靠）

---

## 八、配置结构

```rust
#[derive(Debug, Clone, Resource)]
pub struct MemoryConfig {
    /// 压缩触发阈值（token 数）
    pub compression_threshold_tokens: u32,
    
    /// 保留最近 N 轮不压缩
    pub preserve_recent_turns: u32,
    
    /// LLM 摘要目标 token 数
    pub summary_target_tokens: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            compression_threshold_tokens: 8000,
            preserve_recent_turns: 2,
            summary_target_tokens: 1000,
        }
    }
}
```

---

## 九、设计决策

### 1. 话题偏离检测

- **用户主动触发**：用户输入 `/btw` 创建子任务承接新话题
- **结束当前任务**：用户输入 `/finish` 结束当前任务，触发向父任务提交

### 2. 首次总结时机

- Token 数达到阈值时自动触发
- 用户输入 `/summarize` 指令时触发

### 3. Token 估算

- 引入 `tiktoken` 库进行精确计算

### 4. 任务结束判定

| 任务类型 | 结束方式 |
|----------|----------|
| 用户主动偏离话题的任务（`/btw` 创建） | 用户执行 `/finish` |
| Agent 创建的子任务 | 保持原有方式（Agent 判定或评估器） |

---

## 十、与 v1 的差异

| 项目 | v1 | v2 |
|------|----|----|
| Task 创建 | 直接创建，content = 用户输入 | Pending 状态，首次响应后总结确定主题 |
| 记忆分层 | 按轮数（recent_turns 等） | 按 token 数，保留最近 2 轮 |
| 压缩触发 | 轮数阈值 | token 阈值 + 用户指令 |
| 话题隔离 | 无 | 子任务承接无关话题 |
| TaskStatus | 无 Pending | 新增 Pending 状态 |
