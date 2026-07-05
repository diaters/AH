> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验孵化写回：让孵化 Agent 携带知识

> **状态：当前有效**

## 背景与目标

当前经验治理流程在任务终态时可以产出孵化提案（IncubationProposal），并将提案通过 `IncubatedAgentRegistry` 写入 `agents.toml`。但日志与代码分析显示，写回结果是一个**空壳 Agent**：

- `agents.toml` 只记录 `name/model/tags/description/tools`，其中 `description` 来自 `incubation_rationale`，而实际运行中该字段为空字符串。
- 提案文件 `.harness/incubation/proposals/<id>.json` 只保存 `knowledge_candidate_ids`，不保存候选内容。
- 候选的完整 payload 仅存于运行时的 `ExperienceStore` 中，进程退出后内容丢失。

本次设计目标是：**让孵化出的 Agent 真正携带可复用的知识**，同时保证知识内容持久化、可审计、不污染仓库配置。

## 当前问题

1. **孵化 Agent 没有知识注入**
   - `writeback_incubation_proposal` 只把 `proposed_agent_profile` 和 `incubation_rationale` 写入 `agents.toml`。
   - `knowledge_candidate_ids` 中的知识没有被转化为 Agent 可用资产。

2. **提案文件只存引用**
   - `IncubationProposal` 中 `knowledge_candidate_ids` 是 UUID 列表。
   - 进程重启后，这些 UUID 指向的 `ExperienceCandidate` 已不存在，提案文件变成“悬空索引”。

3. **仓库 `agents.toml` 被测试污染**
   - 当前 `agents.toml` 出现 `incubated-test` / `model = "test"` 这类测试固件。
   - 测试直接写仓库配置文件，导致运行环境状态与测试结果混杂。

## 方案选择

### 方案 A：在 Proposal 里存内容，并把摘要写进 description

把 `IncubationProposal` 扩展为同时保存每个候选的 `title` 和 `payload.content`，写回 `agents.toml` 时把标题和关键洞察拼成 `description`。

- 优点：改动最小，复用现有 proposal 文件和 `agents.toml`。
- 缺点：`description` 字段不适合承载结构化知识；知识没有独立复用价值。

### 方案 B：独立知识资产文件

新增 `.harness/incubation/knowledge/<agent_name>/<candidate_id>.json`，让知识和 Agent 元数据分离，Agent 启动时自动加载。

- 优点：结构清晰，知识可被多 Agent 引用。
- 缺点：需要新增目录、加载逻辑和生命周期管理，改动面大。

### 方案 C：复用 LTM 体系（推荐）

当知识候选被合并进孵化提案时，同步调用现有 `LongTermMemoryService.add_entry`，把候选内容写入 `.harness/memory/agents/<agent_name>.json`。`agents.toml` 的 `description` 用候选标题合成一句话摘要。测试改用临时目录。

- 优点：完全复用已有 LTM 基础设施，不引入新概念；新 Agent 启动时自动加载自身 LTM。
- 缺点：LTM 当前按 Agent 私有记忆设计，若未来需要多 Agent 共享孵化知识，可能需要二次迁移。

**结论：选择方案 C。**

## 设计细节

### 1. 数据流变化

```text
子任务完成
    ↓
collector 提交 ExperienceCandidate (Knowledge)
    ↓
ExperienceStore 汇聚到父任务 inbox
    ↓
/finish → ExperienceCollectionWorkItem → collector 提交顶层候选
    ↓
experience_governance_system 把候选分流到 IncubationProposal
    ↓
spawn_incubation_confirmation 调用 merge_into_proposal
    ↓
用户审批 allow_once
    ↓
writeback_incubation_proposal
    ├─ 把候选内容写入目标 Agent 的 LTM 文件
    └─ 把 Agent 元数据写入 agents.toml
```

### 2. LTM 写回点

LTM 写回应放在**用户审批通过之后**的 `writeback_incubation_proposal` 中执行，避免用户拒绝后留下孤儿 LTM 条目。

具体做法：

- 在 `writeback_incubation_proposal` 确认 `proposal.status == Approved` 后、写入 `agents.toml` 前或后，遍历 `proposal.knowledge_candidate_ids`。
- 对每个候选，从 `ExperienceStore.candidates` 取出 `ExperienceCandidatePayload::Knowledge { content, memory_kind }`。
- 转换为 `LongTermMemoryEntry` 并追加到 `.harness/memory/agents/<agent_name>.json`。

若某个候选已在之前写回中落盘（例如由于去重导致该函数被多次触发），`LongTermMemoryService.add_entry` 应通过 `source_candidate_id` 去重，避免重复条目。

**为什么不放在 `merge_into_proposal` 阶段写回？**

`merge_into_proposal` 发生在治理决议后、用户审批前。此时写回 LTM 会导致：用户一旦拒绝，知识内容已残留在目标 Agent 的 LTM 文件中，且 `agents.toml` 中不存在对应 Agent，形成不可见垃圾数据。

### 3. description 生成规则

`writeback_incubation_proposal` 当前把 `incubation_rationale` 作为 `description`。由于 `incubation_rationale` 当前为空，需要补充生成规则：

- 从 `proposal.knowledge_candidate_ids` 中读取候选标题。
- 若只有 1 个候选：`description = candidate.title`。
- 若有多个候选：`description = "基于 <N> 条经验孵化：" + 前 3 个候选标题，用 "；" 分隔`。
- 若全部为空：`description = ""`（保持现状，避免无意义占位）。

描述长度不做硬性截断，因为 `agents.toml` 本身适合短文本。若标题过长，可在实现阶段评估是否需要截断到 200 字符。

### 4. 测试隔离

`IncubatedAgentRegistry::append` 接受 `config_path` 参数，已经为测试隔离留下入口。需要修改的测试：

- `tests/incubation_execution_flow.rs`
- `tests/experience_layered_governance_flow.rs`
- `src/infrastructure/incubation/agent_registry.rs` 自身单元测试

**要求**：所有测试必须写入 `tempfile::TempDir` 生成的路径，不能直接读写仓库根目录的 `agents.toml`。

对于需要验证“Agent 启动时加载 LTM”的集成测试，可以：

1. 在临时目录下创建 `.harness/memory/agents/<agent>.json`。
2. 启动 harness 并加载该临时配置。
3. 断言该 Agent 的 STM/LTM 中包含预期内容。

### 5. 错误处理

- LTM 写回失败应视为整个孵化写回失败：
  - 记录 `warn` 日志。
  - 不写入 `agents.toml`（避免产生空壳 Agent）。
  - 把 proposal 状态置为 `ExecutionFailed`，与现有 `agents.toml` 写回失败逻辑一致。
- 若 `agents.toml` 写回成功但 LTM 写回失败，属于不一致状态，应在日志中明确标记，并在实现时考虑是否需要回滚 `agents.toml`。本设计优先保证：LTM 写回成功后，再执行 `agents.toml` 写回。

### 6. 文件变更范围

主要改动文件：

- `src/domain/contribution.rs`：可能新增 LTM 写回相关消息类型（若采用事件驱动）。
- `src/systems/experience/writeback.rs`：在 `writeback_incubation_proposal` 中增加 LTM 写回逻辑。
- `src/infrastructure/memory/long_term_memory_service.rs`：确保 `add_entry` 支持按 `source_candidate_id` 去重。
- `src/infrastructure/incubation/agent_registry.rs`：description 生成逻辑（或在上层生成后传入 `IncubatedAgentRecord`）。
- 相关测试文件：改用临时目录。

## 验证标准

1. 完成一次 `/finish` 后，新孵化的 Agent 在 `agents.toml` 中有非空 `description`。
2. `.harness/memory/agents/<incubated-agent-name>.json` 存在且包含对应候选的 payload 内容。
3. 重启 harness 后，该孵化 Agent 的 LTM 能被正确加载。
4. 运行测试后，仓库根目录 `agents.toml` 不出现 `incubated-test` 等测试固件。
5. 现有 CI 检查（`cargo test`、`cargo clippy`、`markdownlint`）全部通过。

## 后续方向

- 若孵化知识需要跨 Agent 共享，可再引入 SharedKnowledge 升级流程，将私有 LTM 中的高价值条目提升为公共知识。
- 可考虑让 LLM 为孵化 Agent 生成更专业的 `description` 和 `tags`，而非简单拼接标题。
