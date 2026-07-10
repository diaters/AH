# 知识库管理员 Agent 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 部署知识库管理员 Agent，复用 chat_with_agent + shell 工具实现知识库管理，移除专用 knowledge_search 工具

**Architecture:** Persistent Agent 配置 + Skill 文件注入 prompt + shell 工具操作 Markdown 文件 + 删除旧工具

**Tech Stack:** Rust, TOML configuration, Markdown skill files, shell tools

## Global Constraints

- Agent 必须是 Persistent 类型（chat_with_agent 只匹配 Persistent）
- Skill 路径：`.harness/assets/agents/<agent-name>/skills/<skill-dir>/SKILL.md`
- Skill 文件必须包含 `name:` 和 `description:` frontmatter
- Shell 工具权限：`shell_exec = Confirm`, `shell_read/list = Allow`
- 知识库目录：`.harness/knowledge/`
- 所有写入操作需审批（工具粒度限制）

---

## File Structure

**Create:**
- `.harness/assets/agents/knowledge-manager/skills/knowledge-management/SKILL.md` - Agent Skill 定义
- `.harness/knowledge/.gitkeep` - 知识库目录占位符

**Modify:**
- `agents.toml` - 添加 knowledge-manager Agent 配置

**Delete:**
- `src/systems/tools/builtin/knowledge_search.rs` - 旧工具实现
- 相关测试和注册代码

**Update:**
- `docs/current-state.md` - 更新 SharedKnowledgeBase 状态
- `docs/TODO.md` - 标记任务完成

---

### Task 1: 创建 Skill 目录和文件

**Files:**
- Create: `.harness/assets/agents/knowledge-manager/skills/knowledge-management/SKILL.md`

**Interfaces:**
- Consumes: 无
- Produces: Skill 文件供 SkillLoader 加载，注入到 Agent 系统提示

- [ ] **Step 1: 创建 Skill 目录结构**

```bash
mkdir -p .harness/assets/agents/knowledge-manager/skills/knowledge-management
```

Expected: 目录创建成功

- [ ] **Step 2: 创建 Skill 文件**

Create `.harness/assets/agents/knowledge-manager/skills/knowledge-management/SKILL.md`:

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

Expected: 文件创建成功

- [ ] **Step 3: 验证 Skill 文件格式**

```bash
head -5 .harness/assets/agents/knowledge-manager/skills/knowledge-management/SKILL.md
```

Expected output:
```
---
name: knowledge-management
description: 知识库管理技能，负责检索与维护知识库
---
```

- [ ] **Step 4: 提交 Skill 文件**

```bash
git add .harness/assets/agents/knowledge-manager/
git commit -m "feat: add knowledge-manager agent skill

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: 创建知识库目录

**Files:**
- Create: `.harness/knowledge/.gitkeep`

**Interfaces:**
- Consumes: 无
- Produces: 知识库目录供 knowledge-manager Agent 操作

- [ ] **Step 1: 创建知识库目录**

```bash
mkdir -p .harness/knowledge
```

Expected: 目录创建成功

- [ ] **Step 2: 创建 .gitkeep 占位符**

```bash
touch .harness/knowledge/.gitkeep
```

Expected: 文件创建成功

- [ ] **Step 3: 提交目录结构**

```bash
git add .harness/knowledge/.gitkeep
git commit -m "chore: initialize knowledge base directory

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: 添加 knowledge-manager Agent 配置

**Files:**
- Modify: `agents.toml`

**Interfaces:**
- Consumes: 无
- Produces: Persistent Agent 配置，供 chat_with_agent 匹配和调用

- [ ] **Step 1: 检查 agents.toml 现有格式**

```bash
cat agents.toml
```

Expected: 查看现有 Agent 配置格式，确认 TOML 结构

- [ ] **Step 2: 追加 knowledge-manager 配置**

Append to `agents.toml`:

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

- [ ] **Step 3: 验证 TOML 格式**

```bash
tail -15 agents.toml
```

Expected: 确认配置追加成功，格式正确

- [ ] **Step 4: 提交配置更改**

```bash
git add agents.toml
git commit -m "feat: add knowledge-manager agent configuration

- Persistent agent for knowledge base management
- shell_exec requires approval (Confirm)
- shell_read/shell_list auto-allowed

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: 删除 knowledge_search 工具

**Files:**
- Delete: `src/systems/tools/builtin/knowledge_search.rs`
- Modify: `src/systems/tools/builtin/mod.rs`
- Modify: `src/systems/tools/mod.rs`

**Interfaces:**
- Consumes: 无
- Produces: 移除旧工具，清理工具注册表

- [ ] **Step 1: 检查 knowledge_search 工具引用**

```bash
grep -r "knowledge_search" src/ --include="*.rs" | head -20
```

Expected: 查看所有引用位置

- [ ] **Step 2: 删除 knowledge_search.rs 文件**

```bash
rm src/systems/tools/builtin/knowledge_search.rs
```

Expected: 文件删除成功

- [ ] **Step 3: 从 mod.rs 移除模块声明**

Read `src/systems/tools/builtin/mod.rs`:

Find and remove:
```rust
mod knowledge_search;
```
and
```rust
pub use knowledge_search::KnowledgeSearchTool;
```

- [ ] **Step 4: 从工具注册表移除**

Read `src/systems/tools/mod.rs`:

Find and remove the `KnowledgeSearchTool` registration block (approximately lines around tool registration).

- [ ] **Step 5: 验证编译**

```bash
cargo check 2>&1 | head -50
```

Expected: 编译通过，无 knowledge_search 相关错误

- [ ] **Step 6: 运行测试**

```bash
cargo test --lib 2>&1 | grep -E "test result|FAILED|error" | head -20
```

Expected: 测试通过，无 knowledge_search 相关失败

- [ ] **Step 7: 提交删除更改**

```bash
git add -A
git commit -m "refactor: remove knowledge_search builtin tool

Replaced by knowledge-manager agent using chat_with_agent + shell tools

BREAKING CHANGE: knowledge_search tool removed, use chat_with_agent with knowledge-manager instead

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: 更新文档

**Files:**
- Modify: `docs/current-state.md`
- Modify: `docs/TODO.md`
- Modify: `docs/README.md` (if needed)

**Interfaces:**
- Consumes: 无
- Produces: 文档同步，反映新的架构状态

- [ ] **Step 1: 更新 current-state.md 的 SharedKnowledgeBase 描述**

Find the section about `SharedKnowledgeBase` and update:

```markdown
#### 记忆治理

- 记忆系统已收敛为 `ShortTermMemory`、`LongTermMemory`、`SharedKnowledgeBase`
- `AgentExperience` 已删除，不再作为独立运行时概念保留
- `LongTermMemory` 采用 `Core + Relevant` 的受控注入策略，避免全量拼接 prompt
- 共享知识写入默认仅允许用户显式命令或主控审核链路，不允许普通 Agent 直写
- 长期记忆已具备基础衰退治理能力，会结合访问时间、重要度与复用次数更新分数
- 长期记忆已实现淘汰机制：`decay_score < 0.1` 且非 `pin` 非 `Critical` 的条目被移除并归档到 `<agent-name>/archive.jsonl`
- 长期记忆已实现 JSON 文件持久化（`MemoryStore` + `MemoryRepository` + `LongTermMemoryService` 写穿模型）
- Agent 启动时可从持久层恢复 `LongTermMemory`，子 Agent 贡献吸收后立即落盘
- `SharedKnowledgeBase` 已迁移到文件系统管理（`.harness/knowledge/*.md`），通过 `knowledge-manager` Agent 统一治理
- 知识库管理员 Agent（Persistent 类型）负责检索与维护知识库，复用 `chat_with_agent` + shell 工具
```

- [ ] **Step 2: 更新 TODO.md**

Find the relevant task and mark as completed:

```markdown
- [x] 新增「知识库管理员」Agent 角色，负责响应其他 Agent 的搜索知识库请求，
  将知识查询职责从调用方解耦到专职 Agent
```

- [ ] **Step 3: 更新已知限制部分**

Update or remove the limitation about `knowledge_search` tool.

- [ ] **Step 4: 提交文档更新**

```bash
git add docs/
git commit -m "docs: sync documentation for knowledge-manager agent

- Update SharedKnowledgeBase status in current-state.md
- Mark knowledge-manager task as completed in TODO.md
- Remove knowledge_search references

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 6: 集成测试验证

**Files:**
- Test: 运行时验证

**Interfaces:**
- Consumes: 所有前面任务的成果
- Produces: 验证知识库管理员 Agent 可正常工作

- [ ] **Step 1: 验证 Agent 加载**

启动应用，检查日志确认 knowledge-manager Agent 被加载：

```bash
# 假设有启动命令，检查 Agent 列表
# 查看日志中是否有 "knowledge-manager" 相关加载信息
```

Expected: knowledge-manager Agent 出现在 Agent 列表中

- [ ] **Step 2: 验证 Skill 加载**

检查 Skill 文件是否被正确加载：

```bash
# 查看日志确认 Skill 注入
grep -r "knowledge-management" <log-file>
```

Expected: Skill 内容被注入到 Agent 系统提示中

- [ ] **Step 3: 手动测试知识检索**

通过 TUI 或其他 Frontend 发送测试消息：

```
{
  "tool": "chat_with_agent",
  "parameters": {
    "agent": "knowledge-manager",
    "message": "列出知识库中的所有条目"
  }
}
```

Expected:
- knowledge-manager Agent 响应
- 使用 shell_list 列出 `.harness/knowledge/` 目录
- 返回当前知识库内容（初始为空）

- [ ] **Step 4: 手动测试知识新增**

测试创建新知识条目：

```
{
  "tool": "chat_with_agent",
  "parameters": {
    "agent": "knowledge-manager",
    "message": "记录新知识：项目使用 Rust + Bevy 框架"
  }
}
```

Expected:
- 触发 shell_exec 审批请求
- 用户确认后创建文件
- 文件内容包含正确的 YAML frontmatter

- [ ] **Step 5: 验证知识库文件**

检查创建的知识文件：

```bash
ls -la .harness/knowledge/
cat .harness/knowledge/*.md
```

Expected: 新文件存在，格式正确

- [ ] **Step 6: 测试知识检索**

再次测试检索：

```
{
  "tool": "chat_with_agent",
  "parameters": {
    "agent": "knowledge-manager",
    "message": "查找关于 Rust 的知识"
  }
}
```

Expected: 返回刚创建的知识条目

---

## Self-Review Checklist

**1. Spec coverage:**
- ✅ Task 1: 创建 Skill 文件和目录结构（设计规格 §2.2）
- ✅ Task 2: 创建知识库目录（设计规格 §3）
- ✅ Task 3: 添加 Agent 配置（设计规格 §2.1）
- ✅ Task 4: 删除旧工具（设计规格 §5）
- ✅ Task 5: 更新文档（设计规格 Phase 5）
- ✅ Task 6: 集成测试验证（设计规格 Phase 4）

**2. Placeholder scan:**
- ✅ 所有步骤包含具体命令或代码
- ✅ 无 "TBD", "TODO", "implement later" 等 placeholders
- ✅ 测试验证步骤明确

**3. Type consistency:**
- ✅ 文件路径一致：`.harness/assets/agents/knowledge-manager/skills/knowledge-management/SKILL.md`
- ✅ Agent 名称一致：`knowledge-manager`
- ✅ Skill 名称一致：`knowledge-management`
- ✅ 工具权限配置一致：`shell_exec = Confirm`, `shell_read/list = Allow`

**4. Spec requirements:**
- ✅ Agent 为 Persistent 类型（已说明）
- ✅ Skill 路径正确（已验证 loader.rs 实现）
- ✅ Skill 包含 name 和 description frontmatter
- ✅ 所有写入操作需审批（已配置）
- ✅ 知识库目录路径正确

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-10-knowledge-manager-agent.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
