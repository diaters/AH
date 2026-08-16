# `/skill` 候选治理触发修复设计

> **状态：当前有效**

| 属性 | 值 |
|------|-----|
| 创建日期 | 2026-08-16 |
| 问题等级 | P1（功能闭环断点，创建的 skill 永远不会生效） |
| 证据日志 | `logs/harness_2026-08-15_23-56-36.jsonl`（L501-L607） |
| 相关文档 | `2026-08-10-skill-creation-command-design.md`、`docs/current-state.md` |

## 1. 背景与现象

`/skill 创建一个获取当天新闻的 skill` 全链路的前半段已按
`2026-08-10-skill-creation-command-design.md` 实现（2026-08-15 的四项修复——
`current_skill_dir` 解析、`read_skill_file` 移除、prompt 模板对齐——均已生效）：

```text
/skill 解析 → SkillCreation WorkItem → write_skill_file 写入 2637 字节
  → submit_skill 返回 {"status":"submitted","candidate_id":"4e9cb8b0-…"}
  → skill-creator 回复"已成功创建 daily-news 技能并提交审核"
```

此后日志中再无任何关于该候选的处理：无状态推进、无审批请求、无写回。
候选停留在 `Submitted`，沙盒 `.sandbox/_draft_*` 永远不会被 rename 到正式目录，
`SkillLoader` 永远扫不到 `daily-news`，用户也收不到审批请求。

## 2. 根因分析

设计文档第 3 节数据流在 `submit_skill` 与"确认流程"之间存在断裂——文档假设候选
会自动进入 `NeedsUserApproval`，但未指定由哪个系统在什么时机驱动状态推进。
实现侧的三个断点：

### 2.1 断点 1：SkillCreation WorkItem 完成无处理分支

`src/systems/transform/llm_response.rs:772-986` 的
`match work_item.work_type` 为 Evaluation / Summarization / ExperienceCollection /
ProfileGeneration / SkillUpdate 提供了专门分支，`SkillCreation` 落入 `_ => {}`
空分支——skill-creator 产出最终文本后，无任何候选状态推进或治理请求。

### 2.2 断点 2：治理入口依赖原任务终态

`Submitted → GovernancePending` 的唯一推进入口
`collect_top_level_governance_candidates`
（`src/domain/contribution.rs:393`）由
`experience_collection_completion_system`
（`src/systems/experience/collection.rs:318-332`）调用，而该系统依赖
`ExperienceCollectionCompletedMessage`，其上游是**任务终态**触发的经验收集
WorkItem。`/skill` 是任务进行中的命令，原任务不因 skill 创建完成而终态，
治理链路根本不会启动。

### 2.3 断点 3：任务终态清理会抢先 Discard 候选

`src/systems/transform/task_lifecycle.rs:282-308`：任务终态时候选若仍为
`Submitted` / `GovernancePending`，沙盒被删除、候选置 `Discarded`。即使原任务
日后终态触发经验收集，候选也已不存在。

## 3. 设计决策

| # | 决策点 | 结论 |
|---|--------|------|
| D1 | 治理触发时机 | SkillCreation WorkItem 完成分支（skill-creator 产出最终文本时） |
| D2 | 是否走 ExperienceCollection | 不走。skill-creator 已明确产出候选，无需再让 LLM 提炼经验，直接提升状态并 spawn 治理请求 |
| D3 | 状态推进方式 | 复用 `collect_top_level_governance_candidates`（统一收束入口，避免第二套提升逻辑） |
| D4 | 失败语义 | WorkItem 完成但无候选提交 → WorkItem `fail()`；LLM 执行 Err → WorkItem `fail()`，对齐 SkillUpdate 分支模式 |

选择 D1 的理由：审批确认发生在 LLM 工具循环结束之后，不会打断
skill-creator 的 `write_skill_file → submit_skill` 迭代，也不与沙盒写回产生竞态；
与其他 WorkItem 类型的完成分支模式一致。

## 4. 方案

### 4.1 变更点 1：新增 SkillCreation 完成分支

在 `llm_response.rs:772` 的 `match work_item.work_type` 中新增
`WorkItemType::SkillCreation` 分支（对齐 ExperienceCollection 的结构）：

```rust
WorkItemType::SkillCreation => {
    match &result.result {
        Ok(AgentExecutionOutput {
            content: OutputContent::ToolCalls(_),
            ..
        }) => {
            // 不 continue，让下方 tool calling loop 处理
            // write_skill_file / submit_skill 的后续迭代
        }
        Ok(_) => {
            // 最终文本：判断是否有候选提交
            //（复用 has_experience_submission 对 root_candidates 的检查）
            if had_submission {
                // 1) 复用统一收束入口：root 候选 Submitted → GovernancePending
                //    store.collect_top_level_governance_candidates(task_id);
                // 2) spawn ExperienceGovernanceRequestMessage {
                //        task_id,
                //        agent_id: SkillCreationContext.agent_id（任务创建者）,
                //    }
                // 3) WorkItem complete() + OnWorkItemCompleted hook
            } else {
                // skill-creator 结束但从未成功 submit_skill
                // WorkItem fail() + OnWorkItemFailed hook
            }
            // 4) despawn WorkItem entity
            // 5) 不 continue：最终文本继续走通用文本路径
            //    （STM entry、multi_turn → Waiting(User)、FrontendOutputText），
            //    与现状 fall-through 行为一致，用户仍能收到创建结果回复
        }
        Err(_) => {
            // 对齐 SkillUpdate 错误路径：fail + hook + despawn + continue
        }
    }
}
```

要点：

- **治理链路无需新系统**：`experience_governance_system` 已消费
  `ExperienceGovernanceRequestMessage` 并按
  `governance_candidates_for_task`（过滤 `GovernancePending`，
  `src/domain/contribution.rs:425-438`）处理；`is_new == true` 的 Skill 候选
  在 `ExperienceKindHint::Skill` 分支被早期拦截路由到
  `ExperienceWritebackDestination::SkillCreation` 且
  `requires_user_confirmation = true`（`src/systems/experience/governance.rs`，
  2026-08-10 设计 N3 修复已实现），后续审批 → 写回 → `SkillRegistry` 注册链路
  均已存在。本设计只补上"按下启动键"的缺失环节。
- **审批路由**：治理产出的 `ToolConfirmationRequestMessage` 按既有审批链路路由到
  任务 `origin_channel`（QQ），满足"审批请求必须回到原会话通道"的约束。
- **`agent_id` 选择**：治理请求的 agent 用
  `SkillCreationContext.agent_id`（任务创建者，即用户对话的 default agent），
  与治理的 governing 语义一致；`index.get_agent` 可解析。is_new 早期拦截
  不依赖 `is_default` 判断，agent 身份不影响路由结果。
- 系统需新增 `Query<&SkillCreationContext>` 参数以在完成分支读取 `agent_id`。

### 4.2 变更点 2：无重复治理说明（无需改动的部分）

任务日后终态触发的 ExperienceCollection 完成时，会再次调用
`collect_top_level_governance_candidates`，但此时本候选已处于
`NeedsUserApproval`（或 `Persisted`），不属于 `Submitted` root 候选，不会被重复
提升或重复治理。无需为 SkillCreation 增加去重逻辑。

### 4.3 变更点 3：终态清理边界确认（无需改动）

治理触发提前到 WorkItem 完成后，`Submitted` 仅存在于 skill-creator 执行期间。
此时若任务终态（用户中断），`task_lifecycle` 将候选 `Discarded` 属正确语义
（未完成的创建流程被放弃）；候选已推进到 `NeedsUserApproval` 后任务终态，
既有 S6 逻辑保留沙盒等待用户确认。清理边界无需调整。

## 5. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/systems/transform/llm_response.rs` | 修改 | 新增 `SkillCreation` 完成分支 + `SkillCreationContext` Query 参数 |

## 6. 验证方案

### 6.1 单元测试

- 有候选提交：WorkItem 完成后 root 候选状态 `Submitted → GovernancePending`，
  spawn `ExperienceGovernanceRequestMessage`，WorkItem `complete`。
- 无候选提交：WorkItem `fail`，不 spawn 治理请求。
- LLM Err：WorkItem `fail`，候选状态保持 `Submitted`（交由终态清理 Discard）。

### 6.2 集成测试

- **闭环主链路**：`/skill intent` → skill-creator `write_skill_file` +
  `submit_skill` → WorkItem 完成分支 → 候选 `GovernancePending` →
  `experience_governance_system` → 候选 `NeedsUserApproval` +
  审批请求路由到 `origin_channel` → 模拟用户批准 → 沙盒 rename 到正式目录 +
  `SkillRegistry` 注册 → 候选 `Persisted` → 下一次 `load_skills` 可见新 skill。
- **任务中断清理**：skill-creator 执行中任务终态 → 候选 `Discarded`、沙盒删除
  （回归确认既有行为）。
- **审批期间任务终态**：候选 `NeedsUserApproval` 时任务终态 → 沙盒保留，
  批准后写回仍成功（既有 S6 行为回归）。

### 6.3 手动验证

- QQ 发送 `/skill 创建一个 XX 的 skill`，确认收到审批请求（原通道），
  批准后 `.harness/assets/agents/<agent>/skills/<skill>/` 存在，
  新任务中该 skill 出现在 Agent prompt。

## 7. 风险与边界

- 完成分支的 Ok(Text) 路径**不 `continue`**、让结果继续走通用文本路径，依赖
  通用路径不重复 despawn WorkItem entity（该 entity 已在分支内 despawn）。
  实现时需验证通用路径对"WorkItem 已 despawn 的结果实体"的容忍性，
  若通用路径会再次操作 WorkItem entity 则需调整 despawn 时序（留到实现计划确认）。
- `has_experience_submission` 同时检查 inbox 候选，SkillCreation 场景下 task 的
  root 候选即 skill 候选，语义兼容；若后续出现"同任务多来源候选"需要收紧为
  仅检查 Skill payload 候选，属后续迭代。
- 本设计不改动 `2026-08-10-skill-creation-command-design.md` 的其余结论；
  该文档第 3 节数据流中"确认流程"一步由本设计补全触发机制，两文档配套阅读。
