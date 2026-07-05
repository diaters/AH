> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验治理模块参数与概念简化设计

> **状态：当前有效**

## 背景与目标

当前经验治理模块已打通全链路流程，但提交工具的参数设计存在根本性问题：

1. **payload 无结构**：`submit_experience_candidate` 的 `payload` 是无结构 JSON 对象，LLM 需要根据 `kind_hint` 猜测应填哪些字段，不同 kind 下必需字段完全不同
2. **伪精细控制面**：`risk_level`/`risk_reason`/`suggested_confirmation` 由 LLM 填写，但治理系统实际根据 `kind_hint` + `is_default_agent` 做分流，完全不依赖这些参数
3. **概念冗余**：`LongTermMemoryKind`（Fact/Constraint/Strategy/AntiPattern/Preference）分类对 LLM 不可靠，对使用效果提升有限
4. **kind_hint 语义矛盾**：`discard` 意味着"不提交"，`shared_knowledge` 应由治理系统判定而非 LLM 声明
5. **skill 未对齐行业标准**：当前 `executable` 的 intent/when_to_use 与 Agent Skills 开放规范（agentskills.io）不兼容

本次设计目标：**从最终输出反推输入参数，全链路简化概念，对齐 Agent Skills 规范**。

## 方案选择

### 方案一：全链路概念清理（已选择）

从提交工具参数开始，沿数据流逐步清理所有不再需要的概念：工具参数 → Submission 数据结构 → 候选载荷 → 治理分流 → 写回执行。

**选择理由**：概念一致，不留僵尸代码；当前测试覆盖充分，适合一次性完成。

### 方案二：参数层优先，领域模型延后

只改工具参数和 Submission 解析，领域模型暂时保留但标记 deprecated。

**未选择理由**：新旧概念并存，治理和写回层无法真正简化。

### 方案三：最小参数修正

只修正工具 JSON Schema，不改内部模型。

**未选择理由**：治标不治本。

## 设计详情

### 一、提交工具参数

当前 JSON Schema：

```json
{
  "title": "string (required)",
  "kind_hint": "enum [knowledge, executable, shared_knowledge, discard]",
  "payload": "object (无结构)",
  "dependency_refs": "array<string>"
}
```

新 JSON Schema：

```json
{
  "title": {
    "type": "string",
    "description": "简明标题，概括此经验的核心要点"
  },
  "kind": {
    "type": "string",
    "enum": ["knowledge", "skill"],
    "description": "经验类型：knowledge=可复用知识，skill=可复用技能包"
  },
  "content": {
    "type": "string",
    "description": "knowledge 类的经验正文"
  },
  "skill_description": {
    "type": "string",
    "description": "skill 类的简要描述，说明做什么+何时触发（对应 Agent Skills 规范的 description）"
  },
  "instructions": {
    "type": "string",
    "description": "skill 类的分步指令正文（对应 Agent Skills 规范的 SKILL.md 正文）"
  },
  "file_refs": {
    "type": "array",
    "items": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "文件路径（绝对路径或相对于项目根目录的相对路径）"
        },
        "role": {
          "type": "string",
          "enum": ["script", "reference", "asset"],
          "description": "文件角色，默认根据扩展名自动推断（.sh/.py→script, .md/.txt→reference, 其余→asset）"
        }
      },
      "required": ["path"]
    },
    "description": "skill 关联的资源文件列表"
  },
  "required": ["title", "kind"]
}
```

变更要点：

- `kind_hint` → `kind`，值域简化为 `knowledge` | `skill`
- 移除 `payload`（无结构 JSON），改为 `content`（knowledge）/ `skill_description` + `instructions`（skill）
- 新增 `file_refs`：结构化文件引用，每项包含 `path` + 可选 `role`
- 移除 `dependency_refs`、`risk_level`、`risk_reason`、`suggested_confirmation`
- `kind=knowledge` 时 `content` 必填；`kind=skill` 时 `skill_description` + `instructions` 必填

文件存在性验证：提交时检查 `file_refs` 中每个文件是否存在。若存在缺失文件，拒绝提交并返回错误信息，列出所有缺失文件路径，要求 LLM 修正后重新提交。

### 二、领域模型变更

#### ExperienceKindHint 简化

```rust
// 之前
enum ExperienceKindHint {
    Knowledge,
    Executable,
    SharedKnowledge,
    Discard,
}

// 之后
enum ExperienceKindHint {
    Knowledge,
    Skill,
}
```

#### ExperienceCandidatePayload 简化

```rust
// 之前
enum ExperienceCandidatePayload {
    Knowledge { content: String, memory_kind: LongTermMemoryKind },
    Executable { intent: String, when_to_use: String, asset_refs: Vec<String> },
}

// 之后
enum ExperienceCandidatePayload {
    Knowledge { content: String },
    Skill {
        name: String,
        description: String,
        instructions: String,
        file_refs: Vec<SkillFileRef>,
    },
}

struct SkillFileRef {
    path: String,
    role: SkillFileRole,
}

enum SkillFileRole {
    Script,
    Reference,
    Asset,
}
```

#### 移除 LongTermMemoryKind

移除 `LongTermMemoryKind` 枚举（Constraint/Preference/Strategy/Fact/AntiPattern），`LongTermMemoryEntry` 不再包含 `kind` 字段。

#### ExperienceCandidateSubmission 简化

```rust
// 之前
struct ExperienceCandidateSubmission {
    title: String,
    kind_hint: ExperienceKindHint,
    payload: serde_json::Value,
    dependency_refs: Vec<String>,
    risk_level: String,
    risk_reason: String,
    suggested_confirmation: Option<String>,
}

// 之后
struct ExperienceCandidateSubmission {
    title: String,
    kind: ExperienceKindHint,
    content: Option<String>,
    skill_description: Option<String>,
    instructions: Option<String>,
    file_refs: Vec<SkillFileRef>,
}
```

#### ExperienceCandidate 简化

移除字段：`risk_level`、`risk_reason`、`suggested_confirmation`。

#### 移除的类型

- `LongTermMemoryKind` 枚举
- `ExperienceRiskLevel` 枚举（简化后的治理逻辑完全不使用，直接删除）
- `ExperienceConfirmationPolicy` 枚举（从候选字段移除，治理系统内部仍需确认策略概念，但不再作为独立枚举暴露，改为治理函数内部的局部逻辑）
- `SharedKnowledgeUpgradeCandidate` 结构体
- `SharedKnowledgeUpgradeQueue` Resource
- `ExperienceKindHint::SharedKnowledge` 和 `ExperienceKindHint::Discard` 变体
- `ExperienceWritebackDestination::SharedKnowledgeUpgrade` 变体

#### 新增的类型

- `SkillFileRef` 结构体（归属 `contribution.rs`）：`{ path: String, role: SkillFileRole }`
- `SkillFileRole` 枚举（归属 `contribution.rs`）：`Script | Reference | Asset`

#### SharedKnowledgeEntry 调整

移除 `LongTermMemoryKind` 后，`SharedKnowledgeEntry` 中的 `kind` 字段改为 `String` 类型，存储自由格式的知识分类标签（如 "fact"、"constraint"），不再绑定枚举。`SharedKnowledgeEntry::candidate()` 方法不再接受 `kind` 参数，默认 `kind` 为 `"fact"`。

### 三、治理分流简化

当前治理根据 `kind_hint` + `is_default_agent` + `risk_level` 做分流，简化后仅根据 `kind` + `is_default_agent`：

| 候选类型 | Agent 类型 | 去向 | 确认策略 |
|---------|-----------|------|---------|
| knowledge | 非 default | LongTermMemory | 无需确认 |
| knowledge | default | IncubationProposal | 需用户确认 |
| skill | 非 default | SkillPackage | 需用户确认 |
| skill | default | IncubationProposal | 需用户确认 |

治理分流伪代码：

```rust
fn govern(candidate: &ExperienceCandidate, is_default: bool) -> ExperienceGovernanceDecision {
    match candidate.kind_hint {
        ExperienceKindHint::Knowledge => {
            if is_default {
                IncubationProposal { confirmation: User }
            } else {
                LongTermMemory { confirmation: None }
            }
        }
        ExperienceKindHint::Skill => {
            if is_default {
                IncubationProposal { confirmation: User }
            } else {
                SkillPackage { confirmation: User }
            }
        }
    }
}
```

### 四、写回简化

#### ExperienceWritebackDestination 简化

```rust
// 之前
enum ExperienceWritebackDestination {
    LongTermMemory,
    SkillPackage,
    SharedKnowledgeUpgrade,
    IncubationProposal,
    Rejected,
}

// 之后
enum ExperienceWritebackDestination {
    LongTermMemory,
    SkillPackage,
    IncubationProposal,
    Rejected,
}
```

#### Skill Package 写回产出

对齐 Agent Skills 规范（agentskills.io），生成标准目录结构：

```text
<agent-skill-dir>/<skill-name>/
├── SKILL.md          # YAML frontmatter (name, description) + instructions 正文
├── scripts/          # role=Script 的文件
├── references/       # role=Reference 的文件
└── assets/           # role=Asset 的文件
```

SKILL.md 生成模板（`<skill-name>` 来自候选的 `title` 字段，转为小写连字符格式）：

```markdown
---
name: <skill-name>
description: <skill_description>
---

<instructions>

## 可用资源

- `scripts/<filename>` — <Script 类型的文件>
- `references/<filename>` — <Reference 类型的文件>
```

#### Knowledge 写回简化

`LongTermMemoryEntry` 不再包含 `kind` 字段，写回时无需区分 Fact/Constraint/Strategy 等。

### 五、删除清单

#### domain 层

- `LongTermMemoryKind` 枚举及所有使用点
- `ExperienceRiskLevel` 枚举（直接删除，治理逻辑不再使用）
- `ExperienceConfirmationPolicy` 枚举（直接删除，确认策略改为治理函数内部局部逻辑）
- `SharedKnowledgeUpgradeCandidate` 结构体
- `SharedKnowledgeUpgradeQueue` Resource
- `ExperienceCandidateSubmission` 中的 `payload`、`risk_level`、`risk_reason`、`suggested_confirmation`、`dependency_refs` 字段
- `ExperienceCandidate` 中的 `risk_level`、`risk_reason`、`suggested_confirmation` 字段
- `ExperienceCandidatePayload::Knowledge` 中的 `memory_kind` 字段
- `ExperienceWritebackDestination::SharedKnowledgeUpgrade` 变体
- `ExperienceKindHint::SharedKnowledge` 和 `ExperienceKindHint::Discard` 变体
- `LongTermMemoryEntry` 中的 `kind` 字段
- `SharedKnowledgeEntry` 中的 `kind: LongTermMemoryKind` 字段
- `SharedKnowledgeEntry::candidate()` 方法中的 `kind` 参数

#### systems 层

- `governance.rs` 中基于 `risk_level` 的条件分支
- `writeback.rs` 中 `writeback_to_shared_knowledge_upgrade` 函数
- `orchestrator.rs` 中 `submission_to_candidate` 的 `payload` 解析逻辑和 `risk_level`/`suggested_confirmation` 映射

### 六、影响分析

#### 需要修改的文件

| 文件 | 变更类型 |
|------|---------|
| `src/domain/contribution.rs` | 重构 ExperienceKindHint、ExperienceCandidatePayload、ExperienceCandidate |
| `src/domain/memory.rs` | 移除 LongTermMemoryKind、LongTermMemoryEntry.kind |
| `src/domain/space.rs` | 移除 SharedKnowledgeUpgrade*、调整 SharedKnowledgeEntry |
| `src/domain/mod.rs` | 更新导出 |
| `src/systems/tools/mod.rs` | 更新工具 JSON Schema |
| `src/systems/tools/builtin/submit_experience_candidate.rs` | 重写参数解析 |
| `src/systems/tools/orchestrator.rs` | 重写 submission_to_candidate |
| `src/systems/experience/governance.rs` | 简化分流逻辑 |
| `src/systems/experience/writeback.rs` | 移除 SharedKnowledgeUpgrade 写回、新增 Skill SKILL.md 写回 |
| `src/systems/experience/collection.rs` | 适配新类型 |
| `src/systems/experience/approval.rs` | 适配新类型 |
| `src/infrastructure/memory/service.rs` | 适配 LongTermMemoryEntry 无 kind |
| `src/plugins/memory.rs` | 移除 SharedKnowledgeUpgradeQueue 注册 |

#### 需要修改的测试

| 测试文件 | 变更 |
|---------|------|
| `tests/experience_candidate_flow.rs` | 适配新类型 |
| `tests/experience_collection_workitem_flow.rs` | 适配新类型 |
| `tests/experience_layered_governance_flow.rs` | 移除 SharedKnowledge 相关断言 |
| `tests/incubation_execution_flow.rs` | 适配新类型 |
| `src/domain/memory.rs` 内联测试 | 移除 LongTermMemoryKind 相关断言 |
| `src/domain/contribution.rs` 内联测试 | 适配新类型 |
| `src/systems/tools/builtin/submit_experience_candidate.rs` 内联测试 | 适配新参数 |

#### 需要更新的文档

- `docs/current-state.md`：更新经验治理模块描述
- `docs/superpowers/specs/2026-06-17-experience-module-refactor-design.md`：归档后更新索引

### 七、数据兼容性

#### MemorySnapshot 迁移

`LongTermMemoryEntry` 移除 `kind` 字段后，已持久化的 `MemorySnapshot` JSON 中仍包含 `kind` 字段。处理策略：

- 反序列化时使用 `#[serde(default)]` 使 `kind` 字段可选，旧数据可正常加载
- 重新写盘时不再包含 `kind` 字段
- `schema_version` 递增为 2，加载 v1 快照时自动迁移

#### ExperienceStore 运行时

`ExperienceStore` 为纯内存 Resource，不涉及持久化，无需迁移。

### 八、错误处理

保持现有策略：

- 写回失败 → 候选状态 `WritebackFailed`
- 孵化执行失败 → proposal 状态 `ExecutionFailed`
- 保留 `warn` 级审计日志
- 新增：`file_refs` 中文件不存在时，拒绝提交并返回错误信息，列出所有缺失文件路径，要求 LLM 修正后重新提交

### 九、验证命令

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
