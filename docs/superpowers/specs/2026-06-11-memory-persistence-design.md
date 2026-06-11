# 长期记忆持久化与跨会话加载设计

## 1. 背景

当前项目中的 `LongTermMemory` 已完成领域模型收敛、`Core + Relevant` 注入和基础衰退治理，
但所有数据仍停留在进程内存态：

- Agent 重启后长期记忆全部丢失
- `MemoryStore` trait 已定义，但没有任何实现者
- `init_agent_memory_system` 当前只会插入空的 `LongTermMemory`
- 子 Agent 贡献吸收后只更新当前运行时，不会写回持久层

这意味着当前的长期记忆只能改善单次进程生命周期内的行为，无法真正承担
“跨任务、跨会话复用稳定经验”的职责。

## 2. 目标

本设计的目标是：

- 为 `LongTermMemory` 提供首个可用的持久化实现
- 让 Agent 在启动时可以从持久层恢复长期记忆
- 让长期记忆的运行时变更能够立即落盘，尽量缩小重启丢失窗口
- 建立清晰的分层边界，避免 ECS 系统直接耦合文件格式和序列化细节
- 为后续替换存储后端保留接口边界

## 3. 非目标

以下内容不在本轮范围内：

- 不为 `SharedKnowledgeBase` 增加持久化
- 不引入 SQLite、向量数据库或远端存储
- 不实现运行时热重载或外部文件监听
- 不处理多进程并发写冲突或文件锁
- 不把 `knowledge_search` trait 化
- 不引入知识库管理员 Agent 或 Agent Skill 体系

## 4. 设计原则

- 运行时状态归 ECS，持久化细节归基础设施层
- 读写路径必须可解释，不隐藏隐式同步行为
- 首版优先最小可用方案，避免过早为远期能力引入复杂度
- 所有长期记忆变更入口尽量收口，避免遗漏持久化
- 跨会话身份必须使用稳定键，不能依赖运行时 UUID

## 5. 方案概览

本次采用 `MemoryStore + MemoryRepository` 的写穿模型：

- `MemoryStore`：底层存储契约，只负责读写持久介质
- `JsonFileMemoryStore`：首个 JSON 文件实现
- `MemoryRepository`：运行时仓储，负责加载、替换、追加和清空 Agent 记忆
- `LongTermMemoryService`：收口运行期长期记忆变更，并在修改内存后立即调用 repository 落盘

该方案不让 ECS system 直接读写文件，而是让 system 依赖 repository 或 service，
从而把文件布局、序列化、错误转换等细节隔离到基础设施层。

## 6. 模块边界

### 6.1 领域层

领域层继续保留：

- `LongTermMemory`
- `LongTermMemoryEntry`
- `MemoryStore` trait

领域层不新增 JSON 结构体、文件路径规则或临时文件写入逻辑。

### 6.2 基础设施层

建议新增目录：

```text
src/
├── infrastructure/
│   └── memory/
│       ├── mod.rs
│       ├── json_file_store.rs
│       └── repository.rs
```

职责划分：

- `json_file_store.rs`：实现 `MemoryStore`，负责目录创建、文件命名、序列化和原子写入
- `repository.rs`：对外暴露按 Agent 读取和写回长期记忆的高层接口

### 6.3 系统层

系统层只负责业务时机，不直接触碰文件系统：

- `init_agent_memory_system`：在 Agent 初始化时加载已有长期记忆
- 贡献吸收和长期记忆新增逻辑：调用 `LongTermMemoryService`
- 清理或淘汰逻辑：在修改内存后统一调用 repository 持久化最新快照

## 7. 持久化身份

`AgentId` 是运行时 UUID，不适合作为跨会话恢复锚点。

因此，首版持久化应以稳定逻辑键作为主键，推荐直接使用：

- `agent.profile.name`

配套规则：

- 文件名使用 `agent.profile.name` 的安全化结果
- JSON 快照中保留原始 `agent_name`
- 运行时 `AgentId` 仍仅用于内存中的实体关联，不参与跨会话索引

## 8. 文件布局与 JSON 格式

### 8.1 存储目录

建议使用项目本地目录：

```text
./.harness/memory/agents/
```

每个 Agent 单独一个文件：

```text
./.harness/memory/agents/<agent_name>.json
```

首版不采用单大文件，以降低局部损坏影响并提升可调试性。

### 8.2 快照结构

JSON 文件不直接裸写 `Vec<LongTermMemoryEntry>`，而是使用带元信息的快照结构：

```json
{
  "agent_name": "summarizer",
  "schema_version": 1,
  "updated_at": "2026-06-11T10:00:00Z",
  "entries": [
    {
      "content": "Always keep summaries concise and task-scoped",
      "kind": "Strategy",
      "scope_tags": ["summarization", "memory"],
      "importance": "High",
      "pin": false,
      "created_at": "2026-06-10T08:00:00Z",
      "last_accessed_at": "2026-06-11T09:30:00Z",
      "reuse_count": 3,
      "decay_score": 0.92,
      "source": "task:123:summarizer",
      "confidence": 0.95
    }
  ]
}
```

字段说明：

- `agent_name`：原始 Agent 名称
- `schema_version`：快照版本，用于后续兼容迁移
- `updated_at`：最后一次成功写盘时间
- `entries`：当前 Agent 的全部长期记忆条目

### 8.3 文件名安全化

`agent_name` 写入文件前应做安全化处理：

- 统一转小写
- 空格替换为下划线
- 移除路径分隔符和其他危险字符

目标是保证任何合法 Agent 名称都不会逃逸目标目录。

## 9. 启动加载流程

### 9.1 初始化时机

`MemoryPlugin` 启动时注册：

- `MemoryRepository` 资源
- 必要的配置资源，例如记忆目录路径

`init_agent_memory_system` 在为 Agent 补 `LongTermMemory` 组件时：

1. 根据 `agent.profile.name` 构造持久化键
2. 调用 repository 加载对应快照
3. 若文件存在且解析成功，则恢复 `entries`
4. 若文件不存在，则插入空 `LongTermMemory`

### 9.2 加载失败策略

- 文件不存在：视为正常空状态，不报错
- JSON 解析失败：记录错误日志，并以空记忆启动
- 目录不存在但可创建：创建后继续运行
- 目录创建失败：启动阶段报错，避免进入伪持久化状态

## 10. 运行期写回流程

### 10.1 写回收口

运行期所有长期记忆修改都应通过统一服务完成，例如：

- `add_entry`
- `absorb_entries`
- `replace_entries`
- `clear`

这些服务函数执行顺序统一为：

1. 修改内存中的 `LongTermMemory`
2. 调用 repository 持久化当前完整快照
3. 记录成功或失败日志

### 10.2 首版写入策略

首版采用“每次变更即落盘”：

- 优点是恢复逻辑简单
- 重启丢失窗口最小
- 不需要引入脏标记、批处理或定时刷盘调度

首版明确不做：

- 周期性快照
- 批量合并写盘
- 写回重试队列

## 11. 原子写入策略

为避免写盘过程中产生半写文件，`JsonFileMemoryStore` 应采用：

1. 先写入 `<agent_name>.json.tmp`
2. 写入和 flush 成功后
3. 再以原子替换方式重命名为 `<agent_name>.json`

这样即使中途崩溃，也更容易保证正式文件始终是上一版完整快照。

## 12. 错误处理

### 12.1 读取错误

读取错误偏恢复性处理：

- 文件不存在：返回空记忆
- 内容损坏：记录 `LongTermMemoryLoadFailed` 日志，并回退为空记忆

原因是读取失败不应阻断整个运行时主链路，但必须可观测。

### 12.2 写入错误

写入错误偏硬失败处理：

- 保留文件路径、Agent 名称、错误原因等上下文
- 返回明确错误给调用方
- 记录 `LongTermMemoryPersistFailed` 日志

原因是系统一旦宣称支持持久化，就不能在写盘失败时静默吞掉错误。

## 13. 日志与可观测性

建议增加以下结构化日志事件：

- `LongTermMemoryLoaded`
- `LongTermMemoryLoadFailed`
- `LongTermMemoryPersisted`
- `LongTermMemoryPersistFailed`

建议包含字段：

- `agent_name`
- `entries_count`
- `file_path`
- `schema_version`
- `error`

## 14. 测试策略

### 14.1 单元测试

应覆盖：

- 文件不存在时返回空快照
- 非法 JSON 时返回可识别错误
- 单 Agent 快照序列化与反序列化可 round-trip
- 临时文件替换后正式文件内容正确
- `agent_name` 安全化规则正确

### 14.2 集成测试

应覆盖：

- Agent 启动时可从 JSON 恢复 `LongTermMemory`
- 子 Agent 贡献吸收后会立即更新对应 JSON 文件
- 重启后再次创建同名 Agent 时，长期记忆仍可恢复

### 14.3 回归测试

必须确认：

- `Core + Relevant` 注入逻辑不受影响
- 现有衰退治理逻辑不受影响
- `SharedKnowledgeBase` 和 `knowledge_search` 不受影响

## 15. 演进空间

本设计为后续能力保留了边界，但不提前实现：

- 将 `JsonFileMemoryStore` 替换为 SQLite 实现
- 为 `SharedKnowledgeBase` 增加独立持久化仓储
- 在 repository 之上增加检索策略抽象
- 为经验贡献和 Agent Skill 资产复用相同的持久化框架

## 16. 结论

首版长期记忆持久化采用本地 JSON 文件方案，并以 `MemoryStore + MemoryRepository`
建立稳定边界：

- Agent 启动时按稳定 `agent.profile.name` 加载历史长期记忆
- 运行期所有长期记忆变更通过统一服务收口并立即落盘
- 文件按 Agent 分片存储，采用带 `schema_version` 的快照结构
- 读取失败以恢复为主，写入失败显式暴露

该方案能够以较低复杂度补齐长期记忆的跨会话闭环，并为后续知识检索和更复杂的记忆资产治理保留扩展空间。
