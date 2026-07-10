# 知识库管理员 Agent 设计规格

## 状态
当前有效

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
知识库管理员 Agent (带 Skill)
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
shell_exec = "Allow"
shell_start = "Allow"
shell_read = "Allow"
shell_list = "Allow"
chat_with_agent = "Allow"
```

**权限说明：**
- `shell_exec`：创建、更新、删除知识文件
- `shell_read`/`shell_list`：检索知识内容
- `chat_with_agent`：潜在的其他 Agent 协作能力

#### 2. Agent Skill 定义

文件位置：`.harness/skills/knowledge-manager/SKILL.md`

**Skill 职责：**
- 定义知识库文件格式（Markdown + YAML Front Matter）
- 提供检索、新增、更新、删除知识的操作指南
- 规范化响应格式

**核心内容：**

```markdown
# Knowledge Management Skill

## 职责
你是知识库管理员，负责知识库的检索与维护。

## 知识库位置
`.harness/knowledge/` 目录

## 文件格式
每个知识条目为一个 Markdown 文件：

---
status: Approved
source: UserCommand | BrainReview | Agent
created_at: YYYY-MM-DD
tags: [tag1, tag2]
---

# 条目标题

内容...

## 操作指南

### 检索知识
使用 shell 工具：
- `grep -r "关键词" .harness/knowledge/` - 全文搜索
- `ls -la .harness/knowledge/` - 列出所有条目
- `cat .harness/knowledge/xxx.md` - 读取完整内容

### 新增知识
1. 创建新文件：`shell_exec` 执行 `cat > .harness/knowledge/new-entry.md << 'EOF'`
2. 内容必须包含 YAML Front Matter
3. 无需审批

### 更新知识
1. 先读取现有内容
2. 使用 `shell_exec` 覆盖文件
3. ⚠️ 需要用户审批

### 删除知识
1. 使用 `shell_exec rm .harness/knowledge/xxx.md`
2. ⚠️ 需要用户审批

## 响应格式
检索结果以结构化 JSON 返回：

{
  "query": "搜索关键词",
  "results": [
    {"title": "条目标题", "file": "xxx.md", "snippet": "摘要..."}
  ],
  "count": 1
}
```

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

**文件格式（Markdown + YAML Front Matter）：**
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
| 新增知识 | ❌ 否 | 建设性行为，低风险 |
| 更新知识 | ✅ 是 | 可能破坏现有信息 |
| 删除知识 | ✅ 是 | 破坏性行为，不可逆 |

**实现方式：**
- 通过 shell 工具的现有审批机制
- Agent Skill 中明确提示需要审批的操作

#### 5. 现有工具处理

**移除清单：**
- `src/systems/tools/builtin/knowledge_search.rs` - 删除文件
- `ToolContext.knowledge` 字段 - 可选移除或保留用于其他用途
- `SharedKnowledgeBase` 资源 - 改为文件系统管理或废弃

**迁移路径：**
1. 导出现有内存数据到 `.harness/knowledge/*.md`
2. 部署知识库管理员 Agent 和 Skill
3. 删除 `knowledge_search` 工具
4. 更新文档和测试

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
1. 知识库管理员接收到消息
2. Skill 指导使用 `grep -r "shell" .harness/knowledge/`
3. LLM 解析结果并结构化返回
4. 支持多轮澄清（如查询模糊）

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
2. 自动生成 YAML Front Matter
3. 无需审批，直接写入
4. 返回确认信息

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
1. 知识库管理员读取现有文件
2. 生成更新后的内容
3. 触发用户审批（通过 shell 工具审批机制）
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
2. 触发用户审批
3. 用户确认后执行 `rm` 命令

## 技术细节

### Skill 文件位置

Agent Skill 通过 `SkillLoader` 从 `.harness/skills/<agent-name>/SKILL.md` 加载：
- 启动时自动扫描
- 内容注入到 Agent 系统提示
- 支持热重载（未来）

### Shell 工具权限

知识库管理员需要的权限：
- `shell_exec`：创建、更新、删除文件
- `shell_read`：读取文件内容（异步会话）
- `shell_list`：列出目录内容

**安全考虑：**
- Agent 只能访问 `.harness/knowledge/` 目录（通过 Skill 约束）
- 实际隔离依赖于 shell 工具的全局权限模型
- 未来可考虑 chroot 或沙箱机制

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
- **例外情况**：如果治理系统需要批量写入大量条目，可临时直接写入文件系统，事后通知知识库管理员重建索引

#### 记忆系统

- `LongTermMemory`：Agent 私有，JSON 文件存储（保持不变）
- `SharedKnowledgeBase`：共享知识，Markdown 文件存储（本设计）

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
- ✅ **语义诚实**：工具职责明确，不暴露伪精细控制
- ✅ **真实需求**：基于实际使用场景设计

## 实施计划

### Phase 1: 准备工作
1. 创建 `.harness/knowledge/` 目录
2. 导出现有 `SharedKnowledgeBase` 数据到 Markdown 文件
3. 创建知识库管理员 Skill 文件

### Phase 2: Agent 部署
1. 在 `agents.toml` 中添加 `knowledge-manager` 配置
2. 配置工具权限
3. 验证 Skill 加载

### Phase 3: 工具移除
1. 删除 `knowledge_search` 工具
2. 更新相关测试
3. 清理 `ToolContext.knowledge`（可选）

### Phase 4: 集成测试
1. 测试知识检索流程
2. 测试知识新增、更新、删除流程
3. 验证审批机制
4. 性能测试（大知识库场景）

### Phase 5: 文档更新
1. 更新 `docs/current-state.md`
2. 更新 `docs/configuration.md`
3. 归档相关设计文档

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

## 参考文档

- `docs/current-state.md` - 当前状态
- `docs/design/2026-06-06-workitem-boundary-design.md` - WorkItem 边界
- `src/systems/tools/builtin/chat_with_agent.rs` - chat_with_agent 实现
- `src/domain/space.rs` - SharedKnowledgeBase 定义
- `agents.toml` - Agent 配置示例
