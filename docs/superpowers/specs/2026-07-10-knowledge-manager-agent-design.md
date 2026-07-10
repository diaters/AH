# 知识库管理员 Agent 设计规格

## 状态
草案

## 概述

引入「知识库管理员」Agent 角色，通过复用现有的 `chat_with_agent` 工具和 shell 工具链路，实现知识库的智能检索与维护，替换现有的专用 `knowledge_search` 内置工具。

## 设计动机

### 当前问题

- `knowledge_search` 是专用内置工具，所有 Agent 直接调用
- `SharedKnowledgeBase` 是进程内存态，无法通过 shell 工具访问
- 知识检索逻辑硬编码（简单关键词匹配），无法智能增强
- 知识管理职责分散，缺乏统一治理入口

### 目标

- **复用已有模块** - 使用 `chat_with_agent` + shell 工具，零新机制
- **智能增强** - 通过 LLM 实现语义理解、多轮澄清、结果筛选
- **统一治理** - 所有知识操作集中到一个 Agent
- **透明可审计** - 所有操作通过 shell 有日志记录
- **人类可读** - 知识库使用 Markdown 文件存储

## 架构设计

### 核心流程

```
Agent 调用 chat_with_agent
    ↓
知识库管理员 Agent (带 Skill, Persistent 类型)
    ↓
shell 工具操作文件系统
    ↓
.harness/knowledge/*.md (Markdown + YAML Front Matter)
```

### 组件清单

#### 1. 知识库管理员 Agent 配置

文件位置：`agents.toml`

```toml
[[agent]]
name = "knowledge-manager"
model = "gpt-4.1-mini"
tags = ["knowledge", "management"]
description = "知识库管理员，负责知识检索与维护"

[agent.tools]
default_permission = "Deny"
shell_exec = "Confirm"
shell_read = "Allow"
shell_list = "Allow"
```

**关键说明：**
- **Agent 类型**：必须是 `Persistent` 类型，因为 `chat_with_agent` 只匹配 `AgentKind::Persistent` 的 Agent（参见 `src/systems/dispatch/agent_selection.rs`）
- **工具权限策略**：
  - `shell_exec = Confirm`：所有写入操作（创建/更新/删除）都需要用户审批
  - `shell_read/shell_list = Allow`：检索操作免批，保证查询效率
  - 移除 `chat_with_agent` 权限：管理员是被动调用角色，无需向外协作能力（最小权限原则）

**限制说明：**
由于 shell 工具权限是**工具粒度**而非命令内容粒度，无法实现"新增免批 / 更新删除需批"的差异化审批。当前方案选择：
- ✅ 所有写入操作统一审批（简化设计，保证安全性）
- ❌ 放弃"新增免批"（与"零新机制"目标权衡后的决策）

#### 2. Agent Skill 定义

**Skill 文件位置：**
```
.harness/assets/agents/knowledge-manager/skills/knowledge-management/SKILL.md
```

**路径说明：**
- 基础路径：`.harness/assets/agents/`（参见 `src/infrastructure/skills/loader.rs` L38）
- Agent 目录：`knowledge-manager/`
- Skills 子目录：`skills/`
- Skill 目录：`knowledge-management/`（可自定义名称）
- 文件名：`SKILL.md`（固定）

**Skill 文件格式：**

Skill 文件必须包含 YAML frontmatter（`name:` 和 `description:` 字段），正文作为 `instructions`。

```markdown
---
name: knowledge-management
description: 知识库管理技能，负责检索与维护知识库
---

## 职责
你是知识库管理员，负责知识库的检索与维护。

## 知识库位置
`.harness/knowledge/` 目录

## 知识条目文件格式
每个知识条目为一个 Markdown 文件，包含 YAML frontmatter：

---
status: Approved
source: UserCommand
created_at: 2026-07-10
tags: [shell, usage]
---

# 条目标题

内容...

**元数据字段：**
- `status`：条目状态（`Approved` | `Candidate` | `Deprecated`）
- `source`：来源（`UserCommand` | `BrainReview` | `Agent`）
- `created_at`：创建日期（ISO 8601）
- `tags`：标签数组（用于分类）

## 操作指南

### 检索知识
使用 shell 工具：
- `grep -r "关键词" .harness/knowledge/` - 全文搜索
- `ls -la .harness/knowledge/` - 列出所有条目
- `cat .harness/knowledge/xxx.md` - 读取完整内容

**注意**：检索操作使用 `shell_read`/`shell_list`，无需审批。

### 新增知识
1. 使用 `shell_exec` 创建新文件：
   ```bash
   cat > .harness/knowledge/new-entry.md << 'EOF'
   ---
   status: Approved
   source: Agent
   created_at: 2026-07-10
   tags: [tag1, tag2]
   ---

   # 条目标题

   内容...
   EOF
   ```
2. **需要用户审批**（`shell_exec = Confirm`）

### 更新知识
1. 先使用 `shell_read` 读取现有内容
2. 使用 `shell_exec` 覆盖文件
3. **需要用户审批**

### 删除知识
1. 使用 `shell_exec rm .harness/knowledge/xxx.md`
2. **需要用户审批**

## 响应格式约定

**注意：以下格式为约定而非强约束。调用方应容错解析 LLM 返回的自由文本。**

检索结果建议以结构化格式返回：

{
  "query": "搜索关键词",
  "results": [
    {"title": "条目标题", "file": "xxx.md", "snippet": "摘要..."}
  ],
  "count": 1
}

调用方应：
1. 尝试解析 JSON 格式
2. 若解析失败，直接展示原始文本
3. 不依赖特定字段的存在
```

**关键修正：**
1. ✅ 正确的路径和目录层级
2. ✅ Skill 文件包含 `name:` 和 `description:` frontmatter
3. ✅ 分离"Skill 文件格式"和"知识条目格式"的描述
4. ✅ 明确所有写入操作都需要审批
5. ✅ 明确响应格式为约定而非强约束

#### 3. 知识库存储格式

**目录结构：**
```
.harness/
└── knowledge/
    ├── shell-usage.md
    ├── project-structure.md
    ├── api-design.md
    └── ...
```

**知识条目文件格式（Markdown + YAML Front Matter）：**
```markdown
---
status: Approved
source: UserCommand
created_at: 2026-07-10
tags: [shell, usage, tools]
---

# Shell 工具使用说明

## 概述
项目使用六个意图化 shell 工具...

## 详细说明
- `shell_exec`：阻塞执行
- `shell_start`：异步会话
...
```

**元数据字段：**
- `status`：条目状态（`Approved` | `Candidate` | `Deprecated`）
- `source`：来源（`UserCommand` | `BrainReview` | `Agent`）
- `created_at`：创建日期（ISO 8601）
- `tags`：标签数组（用于分类）

**优势：**
- ✅ 每个文件自包含，便于 shell 工具独立处理
- ✅ 人类可直接阅读和编辑
- ✅ 支持 git 版本控制

#### 4. 审批策略

| 操作 | 是否需要审批 | 原因 |
|------|------------|------|
| 检索知识 | ❌ 否 | 使用 `shell_read`/`shell_list`，配置为 `Allow` |
| 新增知识 | ✅ 是 | 使用 `shell_exec`，配置为 `Confirm` |
| 更新知识 | ✅ 是 | 使用 `shell_exec`，配置为 `Confirm` |
| 删除知识 | ✅ 是 | 使用 `shell_exec`，配置为 `Confirm` |

**设计权衡说明：**
- 由于 shell 工具权限是**工具粒度**（非命令内容粒度），无法实现"新增免批 / 更新删除需批"的差异化审批
- 选择方案：所有写入操作统一审批
- 优势：简化设计，保证安全性，符合"简化优先"原则
- 劣势：新增知识也需要审批，操作成本略高

**审批路由机制：**
- 知识库管理员作为子任务被调用时，继承父任务的 `origin_channel`
- 工具审批请求（`ToolConfirmationRequestMessage`）路由到该通道
- 审批弹出位置：原始用户所在的 Frontend（TUI/Telegram/QQ 等）
- 流程验证：子任务继承父任务通道的机制已存在，审批落地有保障

#### 5. 现有工具处理

**移除清单：**
1. `src/systems/tools/builtin/knowledge_search.rs` - 删除文件
2. `src/systems/tools/builtin/mod.rs` - 移除 `mod knowledge_search` 和 `pub use`
3. 工具注册（`src/systems/tools/mod.rs`）- 移除 `KnowledgeSearchTool` 注册
4. 测试文件 - 移除相关单元测试
5. `ToolContext.knowledge` 字段（`src/domain/space.rs`）- 可选移除或保留

**影响面评估：**
- `ToolContext` 被所有内置工具测试引用：
  - `knowledge_search.rs` 测试（直接依赖）
  - `chat_with_agent.rs` 测试（传递依赖）
  - `shell/exec.rs`、`shell/start.rs` 等测试（传递依赖）
- 移除 `knowledge_search` 后需同步清理：
  - 工具执行器注册表
  - 所有相关测试用例
  - 文档引用

**迁移路径：**
1. 创建 `.harness/knowledge/` 目录（空目录初始化）
2. 手工整理需保留的条目（当前 `SharedKnowledgeBase` 无持久化数据，进程重启即清空）
3. 部署知识库管理员 Agent 和 Skill 文件
4. 删除 `knowledge_search` 工具及相关代码
5. 更新文档和测试

## 使用示例

### 检索知识

**请求：**
```json
{
  "tool": "chat_with_agent",
  "parameters": {
    "agent": "knowledge-manager",
    "message": "查找关于 shell 工具的使用说明"
  }
}
```

**响应流程：**
1. 知识库管理员接收到消息（作为 Persistent Agent）
2. Skill 指导使用 `shell_list` 和 `shell_read` 检索（无需审批）
3. LLM 解析结果并返回（格式为约定，调用方应容错解析）
4. 支持多轮澄清（如查询模糊）

**预期响应（约定格式）：**
```json
{
  "query": "shell 工具",
  "results": [
    {
      "title": "Shell 工具使用说明",
      "file": "shell-usage.md",
      "snippet": "项目使用六个意图化 shell 工具..."
    }
  ],
  "count": 1
}
```

**容错策略：**
若 LLM 返回非 JSON 格式，调用方直接展示原始文本。

### 新增知识

**请求：**
```json
{
  "tool": "chat_with_agent",
  "parameters": {
    "agent": "knowledge-manager",
    "message": "记录新知识：项目使用 Rust + Bevy 框架，采用 ECS 架构"
  }
}
```

**响应流程：**
1. 知识库管理员创建新文件 `project-tech-stack.md`
2. 自动生成 YAML frontmatter
3. 触发 `shell_exec` 审批（弹出在原始用户的 Frontend）
4. 用户确认后写入文件
5. 返回确认信息

### 更新知识

**请求：**
```json
{
  "tool": "chat_with_agent",
  "parameters": {
    "agent": "knowledge-manager",
    "message": "更新 shell 工具说明，添加 shell_input 的使用案例"
  }
}
```

**响应流程：**
1. 知识库管理员读取现有文件（`shell_read`，免批）
2. 生成更新后的内容
3. 触发 `shell_exec` 审批
4. 用户确认后写入

### 删除知识

**请求：**
```json
{
  "tool": "chat_with_agent",
  "parameters": {
    "agent": "knowledge-manager",
    "message": "删除过时的 API 文档"
  }
}
```

**响应流程：**
1. 知识库管理员确认要删除的文件
2. 触发 `shell_exec` 审批
3. 用户确认后执行 `rm` 命令

## 技术细节

### Skill 文件位置

Agent Skill 通过 `SkillLoader` 加载：
- 基础路径：`.harness/assets/agents/`
- 扫描路径：`.harness/assets/agents/<agent-name>/skills/<skill-dir>/SKILL.md`
- 启动时自动扫描
- 内容注入到 Agent 系统提示

**加载流程：**
1. `SkillLoader::default_path()` 返回 `.harness/assets/agents/`
2. `load_skills(agent_name)` 扫描 `<agent-name>/skills/` 下所有子目录
3. 每个子目录中的 `SKILL.md` 被解析
4. 解析 frontmatter 中的 `name:` 和 `description:`
5. 正文作为 `instructions` 字段

### Shell 工具权限

知识库管理员需要的权限：
- `shell_exec = Confirm`：创建、更新、删除文件（需审批）
- `shell_read = Allow`：读取文件内容（免批）
- `shell_list = Allow`：列出目录内容（免批）

**安全考虑：**
- Agent 目录访问：通过 Skill prompt 约束为 `.harness/knowledge/`
- **语义诚实**：这是**软约束**，非强制隔离。Shell 工具无沙箱或目录白名单。
- 实际隔离依赖于 shell 工具的全局权限模型
- 未来可考虑 chroot 或沙箱机制（需新增能力）

### 与现有系统集成

#### 经验治理流程

**现有流程：**
```
Task 结束 → 经验候选 → 治理 → LongTermMemory / SharedKnowledgeBase
```

**新流程：**
```
Task 结束 → 经验候选 → 治理 → LongTermMemory / 知识库管理员写入文件
```

**集成点：**
- **推荐方式**：经验治理决策写入知识库时，调用 `chat_with_agent` → 知识库管理员，保持统一入口
- **例外情况**：如果治理系统需要批量写入大量条目，可临时直接写入文件系统，事后通知知识库管理员

#### 记忆系统

- `LongTermMemory`：Agent 私有，JSON 文件存储（保持不变）
- `SharedKnowledgeBase`：共享知识，Markdown 文件存储（本设计）
  - 当前状态：纯内存态，无持久化数据
  - 迁移方式：初始化空知识库，不涉及数据导出

## 优势分析

### 架构优势

| 维度 | 旧方案 | 新方案 |
|------|--------|--------|
| 机制复杂度 | 专用工具 + 内存资源 | 复用 chat_with_agent + shell |
| 智能程度 | 硬编码关键词匹配 | LLM 语义理解 + 多轮澄清 |
| 扩展性 | 需修改代码 | 只需更新 Skill |
| 审计能力 | 内部日志 | Shell 完整日志 |
| 人类可读性 | 内存态，不可见 | Markdown 文件，可直接编辑 |

### 设计原则对齐

- ✅ **简化优先**：零新机制，完全复用已有模块
- ✅ **语义诚实**：明确审批粒度限制、响应格式为约定、目录隔离为软约束
- ✅ **真实需求**：基于实际使用场景设计

## 实施计划

### Phase 1: 准备工作
1. 创建 `.harness/knowledge/` 目录（空目录）
2. 创建 `.harness/assets/agents/knowledge-manager/skills/knowledge-management/SKILL.md`
3. 初始化空知识库（无需导出数据）

### Phase 2: Agent 部署
1. 在 `agents.toml` 中添加 `knowledge-manager` 配置
2. 配置工具权限（`shell_exec = Confirm`, `shell_read/list = Allow`）
3. 验证 Skill 加载（检查 prompt 注入）
4. 验证 Agent 为 Persistent 类型

### Phase 3: 工具移除
1. 删除 `src/systems/tools/builtin/knowledge_search.rs`
2. 更新 `src/systems/tools/builtin/mod.rs`（移除 mod 和 pub use）
3. 移除工具注册（`src/systems/tools/mod.rs`）
4. 清理所有相关测试用例
5. 可选：移除或保留 `ToolContext.knowledge` 字段

### Phase 4: 集成测试
1. 测试知识检索流程（免批）
2. 测试知识新增、更新、删除流程（需审批）
3. 验证审批路由（继承父任务 origin_channel）
4. 验证响应格式容错解析
5. 性能测试（大知识库场景）

### Phase 5: 文档更新
1. 更新 `docs/current-state.md`（SharedKnowledgeBase 状态变化）
2. 更新 `docs/configuration.md`（新增 Agent 与目录约定）
3. 更新 `docs/TODO.md`（标记知识库管理员任务完成）
4. 归档相关设计文档

## 风险与缓解

### 风险 1：Shell 工具性能

**问题：** 大知识库下 `grep` 可能较慢

**缓解：**
- 短期：接受性能开销（知识库通常不大）
- 长期：引入 `ripgrep` 或索引机制

### 风险 2：并发写入冲突

**问题：** 多个任务同时写入知识库

**缓解：**
- 依赖 shell 工具的会话隔离
- 文件系统原子性（写入新文件后 mv）
- 未来可考虑文件锁

### 风险 3：Skill 约束力不足

**问题：** LLM 可能不遵循 Skill 指南

**缓解：**
- Skill 内容清晰明确
- 通过示例和模板引导
- 定期审查操作日志
- 调用方容错解析响应格式

### 风险 4：审批流程用户体验

**问题：** 新增知识也需要审批，操作成本略高

**缓解：**
- 审批请求携带完整上下文（文件内容、操作类型）
- 用户可快速预览并决策
- 未来可考虑"批量审批"或"信任模式"

## 未来扩展

### 短期优化
- 支持知识库索引文件（加速查询）
- 支持知识库备份和恢复
- 支持知识库统计和可视化

### 长期演进
- 引入向量检索（语义搜索）
- 支持知识库版本控制（git 集成）
- 支持多知识库实例（项目级、全局级）
- 支持知识库同步（多实例场景）
- 支持命令内容级审批（需新机制）

## 参考文档

- `docs/current-state.md` - 当前状态
- `docs/design/2026-06-06-workitem-boundary-design.md` - WorkItem 边界
- `src/systems/tools/builtin/chat_with_agent.rs` - chat_with_agent 实现
- `src/domain/space.rs` - SharedKnowledgeBase 定义
- `src/infrastructure/skills/loader.rs` - Skill 加载实现
- `src/systems/dispatch/agent_selection.rs` - Agent 选择逻辑
- `agents.toml` - Agent 配置示例
