> **状态：已归档（2026-06-10）** — 本规格描述的功能已实现。
> 相关能力已记录在 [docs/current-state.md](../../current-state.md)。

# 子任务结果传递机制重设计

日期：2026-05-25
状态：已评审

## 1. 问题分析

### 1.1 现状

子任务系统（`create_tasks` 工具）支持 DAG 依赖关系（`depends_on` 字段），但依赖子任务无法获取兄弟任务的执行结果。具体问题：

1. **`knowledge_search` 始终返回空结果**：`SpaceKnowledge.entries` 仅有两个硬编码条目，无实际数据源
2. **子任务结果无写入路径**：`task_termination_system` 和 `sub_task_completion_system` 不将 `result_summary` 写入 `SpaceKnowledge`
3. **依赖子任务只能通过 `knowledge_search` 检索共享知识**：该工具是唯一检索手段，但数据源为空

### 1.2 日志证据

日志 `harness_2026-05-25_20-52-22.jsonl` 中，"数据比例计算"子任务 5 轮共 10 次 `knowledge_search` 调用全部返回 `{"count": 0, "results": []}`，最终 `ToolCallingLimitExceeded` → `Failed(AgentError)`。

## 2. 设计决策

### 2.1 核心机制：Prompt 注入

选择 **Prompt 注入** 而非共享知识库检索作为子任务间结果传递的核心机制：

- **确定性**：兄弟任务结果直接出现在依赖任务的 prompt 中，无需搜索，不会漏掉
- **无需搜索质量依赖**：不依赖关键词匹配、分词等不确定性因素
- **改动最小**：利用已有的 `SubTaskBatchState`（已存有 `result_summary`）和 `AgentSpawnRequestMessage.task_prompt`

### 2.2 结果精炼：标记对机制

要求子 Agent 在输出末尾用 `<<<RESULT>>>...<<</RESULT>>>` 包围精炼总结：

- 子 Agent 自主控制摘要质量，比 post-hoc summarization 更可靠
- 避免将完整的 LLM 输出（可能数千字）直接注入依赖任务的 prompt
- 标记对格式便于程序化提取，正则匹配即可

### 2.3 SpaceKnowledge 保留但暂不改造

`knowledge_search` 工具保留，但当前阶段不做双写改造。待有真实数据源需求时再独立迭代。

## 3. 详细设计

### 3.1 标记对格式

子任务 system_prompt 中追加指令，要求 LLM 在回答末尾输出：

```
<<<RESULT>>>
精炼的结论或最终答案
<<</RESULT>>>
```

规则：
- 标记对位于回答的最末尾
- 标记内内容应精炼、自包含，便于其他任务引用
- 如果 LLM 输出多对标记，提取**最后一对**
- 非子任务（无 `SubTaskConfig`）不追加指令

### 3.2 标记对提取

在 `llm_response_system` 中，当任务输出 `OutputContent::Text` 且 `task.parent_task_id.is_some()`（即子任务）时：

1. 用正则 `<<<RESULT>>>([\s\S]*?)<<</RESULT>>>` 提取标记对内容
2. 提取成功：标记对内容作为 `result_summary`，原文本保留不变（用户看到完整输出）
3. 提取失败：完整输出文本作为 `result_summary` 的 fallback，同时记录 `warn!` 日志 `ResultMarkerNotFound`

`result_summary` 在 `mark_done` 调用前设置，确保 `task_termination_system` 触发 `SubTaskCompletedMessage` 时已有值。

### 3.3 子任务 system_prompt 注入

在 `brain_dispatch_system` 中构建 `AgentSpawnRequestMessage` 时：

- 检测任务有 `SubTaskConfig`
- 将总结指令追加到 `task_system_prompt` 字段
- 如果 `task_system_prompt` 已有内容，追加而非覆盖

system_prompt 内容：

```
你是一个专注于完成特定子任务的 AI Agent。请仔细阅读任务描述，认真完成分配给你的工作。

重要：请在回答的最后，用 <<<RESULT>>> 和 <<</RESULT>>> 标记包围你的核心结论或最终答案。
标记内的内容应当精炼、自包含，便于其他任务引用。

示例格式：
（你的详细分析和推理过程...）

<<<RESULT>>>
你的精炼结论
<<</RESULT>>>
```

### 3.4 依赖子任务 Prompt 注入

在 `brain_dispatch_system` 中，当子任务 DAG 依赖满足、准备 dispatch 时：

1. 从 `SubTaskBatchState` 中收集 `depends_on` 列表内已完成兄弟任务的 `result_summary`
2. 拼接到 `AgentSpawnRequestMessage.task_prompt` 中

注入格式：

```
{原始 task.content}

## 兄弟任务结果

### {兄弟任务1名称}
{兄弟任务1 result_summary}

### {兄弟任务2名称}
{兄弟任务2 result_summary}

请基于以上兄弟任务的结果完成你的任务。你可以直接引用这些结果，无需重新计算或搜索。
```

规则：
- 只注入 `depends_on` 列表中指定的兄弟任务结果，而非所有已完成任务
- 失败任务的 result_summary 为空时，注入 `[任务名: 执行失败，无结果]`
- 无依赖的子任务不注入兄弟结果段落

## 4. 改动范围

| 文件 | 改动 |
|------|------|
| `src/systems/dispatch.rs` - `brain_dispatch_system` | 1. 注入总结指令到 `task_system_prompt`<br>2. 注入兄弟任务结果到 `task_prompt` |
| `src/systems/transform.rs` - `llm_response_system` | 子任务完成时提取 `<<<RESULT>>>` 标记对，作为 `result_summary` |

## 5. 错误处理与边界情况

| 场景 | 处理 |
|------|------|
| LLM 不遵守标记格式 | `result_summary` fallback 为完整输出文本，`warn!` 日志 `ResultMarkerNotFound` |
| 依赖任务部分失败 | 失败任务注入 `[任务名: 执行失败，无结果]`，依赖任务自行决定如何处理 |
| 多重标记对 | 正则提取最后一对 |
| 非子任务 Task | 不追加总结指令，不做标记对提取，行为不变 |
| result_summary 为空 | fallback 注入失败说明 |
| 依赖链深度 | 每层都通过标记对精炼，避免指数膨胀 |

## 6. 不在本次范围内

- SpaceKnowledge 双写改造
- knowledge_search 工具改进
- 子任务重试策略
- 标记对内容的长度限制
