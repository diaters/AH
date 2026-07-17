//! Brain 分发 System
//!
//! 使用 Brain Agent 进行任务分发决策。
//!
//! ## Brain Agent 选择规则
//!
//! 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的那个。
//! 这允许灵活配置多个 Brain Agent（如不同模型），同时保持确定性选择。

use crate::prelude::*;
use serde::Deserialize;
use tracing::{debug, trace, warn};

use crate::{
    app::{Clock, HarnessSettings},
    contracts::{AgentCapabilitySummary, BrainSelectionPolicy, FirstBrainPolicy},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        AgentSpawnRequestMessage, BatchTaskState, EntryRole, LongTermMemory,
        MessageDispatchedHookPending, ShortTermMemory, SpaceToolRegistry, SubTaskBatchState,
        SubTaskConfig, Task, TaskInjectedSkill, TaskStatus, ToolPermission, WaitingReason,
    },
    infrastructure::skills::{SkillId, SkillRegistry},
};

use super::agent_selection::select_agent_for_sub_task_with_skill;

const SUB_TASK_SYSTEM_PROMPT: &str = "\
你是一个专注于完成特定子任务的 AI Agent。请仔细阅读任务描述，认真完成分配给你的工作。

重要：请在回答的最后，用 <<<RESULT>>> 和 <<</RESULT>>> 标记包围你的核心结论或最终答案。
标记内的内容应当精炼、自包含，便于其他任务引用。

示例格式：
（你的详细分析和推理过程...）

<<<RESULT>>>
你的精炼结论
<<</RESULT>>>";

struct AgentDescription {
    name: String,
    model: String,
    tags: Vec<String>,
    description: String,
}

fn brain_user_prompt_from_descriptions(task_content: &str, agents: &[AgentDescription]) -> String {
    let agent_descriptions: Vec<String> = agents
        .iter()
        .filter(|agent| !agent.tags.contains(&"brain".to_string()))
        .map(|agent| {
            format!(
                "- name: \"{}\"\n  model: \"{}\"\n  tags: {:?}\n  description: \"{}\"",
                agent.name, agent.model, agent.tags, agent.description,
            )
        })
        .collect();

    format!(
        r#"Task content: "{}"

Available agents:
{}

Select the best agent for this task and provide a delegate prompt."#,
        task_content,
        agent_descriptions.join("\n"),
    )
}

/// 构建带历史对话的 prompt（Brain Agent 使用）
fn build_prompt_with_history(task_content: &str, short_term: Option<&ShortTermMemory>) -> String {
    let Some(stm) = short_term else {
        return task_content.to_string();
    };

    if stm.entries.is_empty() {
        return task_content.to_string();
    }

    // 构建历史对话
    let mut history = String::new();

    // 添加摘要前缀（如果有）
    if let Some(summary) = &stm.summary_prefix {
        history.push_str(&format!("[Previous context summary]\n{}\n\n", summary));
    }

    // 添加对话历史
    history.push_str("[Conversation history]\n");
    for entry in &stm.entries {
        let role = match entry.role {
            EntryRole::User => "User",
            EntryRole::Assistant => "Assistant",
            EntryRole::Summary => "System note",
            EntryRole::Archive => continue,
        };
        history.push_str(&format!("{}: {}\n", role, entry.content));
    }

    // 组合成完整 prompt
    format!(
        "{}\n\n[Current request]\n{}",
        history.trim_end(),
        task_content
    )
}

/// 构建子任务的 system prompt，如选中 skill 则拼接 skill instructions
fn build_sub_task_system_prompt(
    skill_id: &Option<SkillId>,
    skill_registry: &SkillRegistry,
) -> String {
    if let Some(skill) = skill_id
        && let Some(entry) = skill_registry.get(skill)
    {
        return format!(
            "{}\n\n## Skill: {}\n\n{}",
            SUB_TASK_SYSTEM_PROMPT, entry.name, entry.instructions
        );
    }
    SUB_TASK_SYSTEM_PROMPT.to_string()
}

/// Brain 分发 System
///
/// 使用 Brain Agent 进行任务分发决策。
///
/// ## Brain Agent 选择
///
/// 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的那个。
#[allow(clippy::too_many_arguments)]
pub fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&ShortTermMemory>,
        Option<&SubTaskConfig>,
    )>,
    agents: Query<&Agent>,
    batch_states: Query<&SubTaskBatchState>,
    registry: Res<SpaceToolRegistry>,
    skill_registry: Res<SkillRegistry>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    // 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的
    let brain_candidates: Vec<AgentCapabilitySummary> = agents
        .iter()
        .filter(|a| {
            a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string())
        })
        .map(AgentCapabilitySummary::from_agent)
        .collect();

    let brain_policy = FirstBrainPolicy;
    let Some(brain_agent_id) = brain_policy.select_brain(&brain_candidates) else {
        debug!(
            event = "BrainAgentNotFound",
            "no brain agent found with 'brain' tag, skipping brain dispatch"
        );
        return;
    };

    let brain_agent = agents.iter().find(|a| a.id == brain_agent_id).unwrap();

    let all_agent_descriptions: Vec<AgentDescription> = agents
        .iter()
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(|agent| AgentDescription {
            name: agent.profile.name.clone(),
            model: agent.profile.model.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        })
        .collect();

    for (task_entity, mut task, short_term, sub_task_config) in &mut tasks {
        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        if task.delegate.is_some() {
            trace!(
                event = "BrainDispatchSkipped",
                task_id = %task.id,
                task_status = ?task.status,
                delegate = ?task.delegate,
                "task already has delegate, skipping brain dispatch"
            );
            continue;
        }

        // 子任务处理：检查 DAG 依赖，由 Brain 分发
        if let Some(config) = sub_task_config {
            // 检查 DAG 依赖是否满足
            let deps_satisfied = if config.depends_on.is_empty() {
                true
            } else if let Some(batch_state) = batch_states
                .iter()
                .find(|bs| bs.batch_id == config.batch_id)
            {
                config.depends_on.iter().all(|dep_name| {
                    batch_state.tasks.get(dep_name).is_some_and(|s| {
                        matches!(s.state, BatchTaskState::Done | BatchTaskState::Failed)
                    })
                })
            } else {
                false
            };

            if !deps_satisfied {
                trace!(
                    event = "SubTaskWaitingForDependencies",
                    task_id = %task.id,
                    child_name = %config.child_agent_name,
                    depends_on = ?config.depends_on,
                    "sub-task waiting for dependencies to complete"
                );
                continue;
            }

            // 收集依赖的兄弟任务结果
            let sibling_results = if !config.depends_on.is_empty() {
                if let Some(batch_state) = batch_states
                    .iter()
                    .find(|bs| bs.batch_id == config.batch_id)
                {
                    let mut results = Vec::new();
                    for dep_name in &config.depends_on {
                        if let Some(status) = batch_state.tasks.get(dep_name) {
                            let result_text = match &status.result_summary {
                                Some(summary) if !summary.is_empty() => summary.clone(),
                                _ => format!("[{}: 执行失败，无结果]", dep_name),
                            };
                            results.push(format!("### {}\n{}", dep_name, result_text));
                        }
                    }
                    if results.is_empty() {
                        None
                    } else {
                        Some(results)
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // 选择匹配的 Agent（基于所需工具标签）
            let child_agent = select_agent_for_sub_task_with_skill(
                agents.iter().map(|a| (a, None::<&LongTermMemory>)),
                &task.content,
                &skill_registry,
            );

            if let Some((agent, _ltm, skill_id)) = child_agent {
                debug!(
                    event = "SubTaskDispatched",
                    task_id = %task.id,
                    child_name = %config.child_agent_name,
                    selected_agent = %agent.profile.name,
                    batch_id = %config.batch_id,
                    "dispatching sub-task to agent"
                );

                let child_task_id = task.id;

                // 注入 TaskInjectedSkill Component（如果选中了 skill）
                if let Some(skill) = &skill_id {
                    commands.entity(task_entity).insert(TaskInjectedSkill {
                        skill_id: Some(skill.clone()),
                    });
                    debug!(
                        event = "TaskInjectedSkillAttached",
                        task_id = %task.id,
                        selected_agent = %agent.profile.name,
                        skill_id = %skill.as_string(),
                        "attached skill to task"
                    );
                }

                commands.spawn(AgentSpawnRequestMessage {
                    parent_agent_id: config.parent_agent_id,
                    task_id: child_task_id,
                    name: config.child_agent_name.clone(),
                    model: config.child_agent_model.clone(),
                    description: config.child_agent_name.clone(),
                    tools: config.allowed_tools.clone(),
                    task_prompt: if let Some(ref results) = sibling_results {
                        format!(
                            "{}\n\n## 兄弟任务结果\n\n{}\n\n请基于以上兄弟任务的结果完成你的任务。你可以直接引用这些结果，无需重新计算或搜索。",
                            task.content,
                            results.join("\n\n")
                        )
                    } else {
                        task.content.clone()
                    },
                    task_system_prompt: Some(build_sub_task_system_prompt(
                        &skill_id,
                        &skill_registry,
                    )),
                });

                if sibling_results.is_some() {
                    debug!(
                        event = "SiblingResultsInjected",
                        task_id = %task.id,
                        child_name = %config.child_agent_name,
                        depends_on = ?config.depends_on,
                        "injected sibling task results into sub-task prompt"
                    );
                }

                task.status = TaskStatus::Waiting(WaitingReason::Agent);
                task.delegate = Some(agent.id);
                task.updated_at = clock.0;
            } else {
                debug!(
                    event = "SubTaskNoAgentAvailable",
                    task_id = %task.id,
                    child_name = %config.child_agent_name,
                    "no suitable agent found for sub-task"
                );
            }
            continue;
        }

        // 构建带历史对话的 prompt
        let prompt_with_history = build_prompt_with_history(&task.content, short_term);
        let prompt =
            brain_user_prompt_from_descriptions(&prompt_with_history, &all_agent_descriptions);

        let stm_entries = short_term.map(|s| s.entries.len()).unwrap_or(0);
        let stm_tokens = short_term.map(|s| s.estimated_tokens).unwrap_or(0);

        debug!(
            event = "BrainDispatch",
            task_id = %task.id,
            task_content = %task.content,
            task_status = ?task.status,
            brain_agent_id = %brain_agent.id,
            brain_agent_name = %brain_agent.profile.name,
            prompt_len = prompt.len(),
            stm_entries = stm_entries,
            stm_tokens = stm_tokens,
            available_agents = ?all_agent_descriptions.iter().map(|a| &a.name).collect::<Vec<_>>(),
            "brain dispatching task"
        );

        // 构建 Brain Agent 可用的工具列表（非 Deny）
        let tools: Vec<_> = registry
            .iter()
            .filter(|tool_def| {
                !matches!(
                    brain_agent.tool_permissions.get_permission(&tool_def.name),
                    ToolPermission::Deny
                )
            })
            .cloned()
            .collect();

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: brain_agent.id,
            request_kind: AgentRequestKind::BrainDecision,
            prompt,
            system_prompt: brain_agent.system_prompt.clone(),
            tools,
            conversation: None,
            work_item_id: None,
            model_override: None,
        };

        task.mark_waiting_for_agent(brain_agent.id, clock.0);
        commands.spawn((
            AgentExecutionRequestMessage { request },
            MessageDispatchedHookPending,
        ));
    }
}

#[allow(dead_code)] // 后续 brain_dispatch 改造任务接入
#[derive(Deserialize)]
struct BrainSkillSelection {
    agent_name: String,
    skill_name: Option<serde_json::Value>,
}

/// 解析 brain LLM 的 skill 选择输出
///
/// 输入 JSON 格式：`{"agent_name": "agent-a", "skill_name": "coding"}`
///
/// 容错策略：
/// - `skill_name` 字段缺失或为 null：返回 None
/// - `skill_name` 为字符串 "None"（不区分大小写）或空字符串（trim 后）：返回 None
/// - `skill_name` 为非字符串类型（数字、布尔、对象、数组）：记录 warn 日志并返回 None
/// - `agent_name` 字段缺失：返回 Err
/// - JSON 格式错误：返回 Err
///
/// 输入清洗：调用 [`sanitize_brain_output`] 剥离 LLM 常见的 markdown 代码块包裹、
/// BOM 与不可见字符，逻辑与 `crate::llm::brain_prompt` 中的私有函数对齐，
/// 但不跨模块复用以避免引入不必要的耦合。
#[allow(dead_code)] // 后续 brain_dispatch 改造任务接入
pub fn parse_brain_skill_selection(raw: &str) -> Result<(String, Option<String>), String> {
    // 清洗 LLM 输出：剥离 markdown 包裹/BOM/不可见字符
    let cleaned = sanitize_brain_output(raw);
    let parsed: BrainSkillSelection = serde_json::from_str(&cleaned)
        .map_err(|e| format!("invalid brain skill selection JSON: {}", e))?;
    let skill = match parsed.skill_name {
        None => None,
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other) => {
            warn!(
                event = "SkillNameParseFailed",
                error_type = "NonStringSkillName",
                error = ?other,
                "skill_name is not a string, treating as None"
            );
            None
        }
    };
    Ok((parsed.agent_name, skill))
}

/// 清洗 brain LLM 输出：剥离 markdown 代码块包裹、BOM 与不可见字符。
///
/// 与 `crate::llm::brain_prompt::sanitize_json_like_input` +
/// `extract_json_block` 逻辑对齐，因后者为私有函数且跨模块复用价值有限，
/// 此处保留本地实现供 brain_dispatch 使用。
fn sanitize_brain_output(raw: &str) -> String {
    let mut s = raw.trim().to_string();

    // 剥离 BOM (U+FEFF)
    if let Some(stripped) = s.strip_prefix('\u{feff}') {
        s = stripped.to_string();
    }

    // 剥离不可见字符 (U+200B / U+200C / U+200D / U+2060)
    for inv in ['\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}'] {
        s = s.replace(inv, "");
    }

    // 剥离 ```json ... ``` 代码块包裹
    let trimmed = s.trim();
    if let (Some(start), Some(end)) = (trimmed.find("```json"), trimmed.rfind("```"))
        && end > start
    {
        let json_start = start + 7;
        return trimmed[json_start..end].trim().to_string();
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{BrainConfig, HarnessConfig},
        domain::{AgentCapabilities, AgentProfile, ChannelId, FrontendKind},
        infrastructure::skills::SkillEntry,
    };
    use uuid::Uuid;

    /// 构建用于测试的 BrainDispatch App（包含必要 Resource 与 System）。
    fn build_test_app() -> App {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.insert_resource(HarnessSettings(HarnessConfig {
            brain: Some(BrainConfig { enabled: true }),
            ..Default::default()
        }));
        app.insert_resource(SpaceToolRegistry::default());
        app.insert_resource(SkillRegistry::default());
        app.add_systems(Update, brain_dispatch_system);
        app
    }

    /// 当 Task 已存在 delegate 时（即便状态为 Ready），BrainDispatch 不应再次派发请求。
    #[test]
    fn brain_dispatch_skips_ready_task_with_existing_delegate() {
        let mut app = build_test_app();

        let brain_agent_id = Uuid::new_v4();
        app.world_mut().spawn(Agent {
            id: brain_agent_id,
            profile: AgentProfile {
                name: "brain".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["brain".to_string()],
                description: "brain agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: Default::default(),
            system_prompt: None,
        });

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test-user".to_string(),
            thread_id: None,
        };
        let mut task = Task::from_user_input_ready("do something", 0, channel);
        task.delegate = Some(Uuid::new_v4());
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.update();

        let request_count = {
            let world = app.world_mut();
            let mut query = world.query::<&AgentExecutionRequestMessage>();
            query.iter(world).count()
        };
        assert_eq!(
            request_count, 0,
            "ready task with delegate should not spawn AgentExecutionRequestMessage"
        );

        let task_after = {
            let world = app.world_mut();
            let mut query = world.query::<&Task>();
            query
                .iter(world)
                .find(|t| t.id == task_id)
                .expect("task should still exist")
                .clone()
        };
        assert_eq!(task_after.status, TaskStatus::Ready);
        assert!(task_after.delegate.is_some());
    }

    mod build_sub_task_system_prompt_tests {
        use super::*;

        fn make_skill_entry(owner: &str, skill_name: &str) -> SkillEntry {
            SkillEntry {
                skill_id: SkillId::new(owner, skill_name),
                name: format!("{}-display", skill_name),
                description: format!("desc for {}", skill_name),
                instructions: format!("INSTRUCTIONS_FOR_{}", skill_name),
                version: 1,
                owner_agent_name: owner.to_string(),
                self_updatable: true,
            }
        }

        #[test]
        fn returns_base_when_skill_id_is_none() {
            let reg = SkillRegistry::default();
            let result = build_sub_task_system_prompt(&None, &reg);
            assert_eq!(result, SUB_TASK_SYSTEM_PROMPT);
        }

        #[test]
        fn includes_skill_when_registry_hit() {
            let mut reg = SkillRegistry::default();
            let entry = make_skill_entry("agent-a", "coding");
            let skill_id = entry.skill_id.clone();
            reg.upsert(entry);

            let result = build_sub_task_system_prompt(&Some(skill_id), &reg);

            assert!(
                result.starts_with(SUB_TASK_SYSTEM_PROMPT),
                "prompt should start with base SUB_TASK_SYSTEM_PROMPT"
            );
            assert!(
                result.contains("## Skill: coding-display"),
                "prompt should include the skill display name section"
            );
            assert!(
                result.contains("INSTRUCTIONS_FOR_coding"),
                "prompt should include the skill instructions"
            );
        }

        #[test]
        fn falls_back_to_base_when_registry_miss() {
            // SkillRegistry::default() 中无该 skill，应回退到 base prompt，不能 panic
            let reg = SkillRegistry::default();
            let missing_skill_id = SkillId::new("agent-a", "missing");

            let result = build_sub_task_system_prompt(&Some(missing_skill_id), &reg);

            assert_eq!(result, SUB_TASK_SYSTEM_PROMPT);
        }
    }

    /// 端到端验证：当 `select_agent_for_sub_task_with_skill` 选中了 skill 时，
    /// `brain_dispatch_system` 会向 task entity 插入 `TaskInjectedSkill { skill_id: Some(...) }`。
    ///
    /// 验证链路：SubTaskConfig 路径 → select_agent_for_sub_task_with_skill → 选中 default agent
    /// （由 list_by_owner 命中预注册的 skill）→ brain_dispatch 注入 TaskInjectedSkill Component。
    #[test]
    fn brain_dispatch_attaches_task_injected_skill_when_skill_selected() {
        let mut app = build_test_app();

        // 给 default-llm-agent 预注册一个 skill
        {
            let mut skill_reg = app.world_mut().resource_mut::<SkillRegistry>();
            skill_reg.upsert(SkillEntry {
                skill_id: SkillId::new("default-llm-agent", "coding"),
                name: "coding".to_string(),
                description: "Coding skill".to_string(),
                instructions: "Always write tests".to_string(),
                version: 1,
                owner_agent_name: "default-llm-agent".to_string(),
                self_updatable: true,
            });
        }

        // Spawn brain agent
        let brain_agent_id = Uuid::new_v4();
        app.world_mut().spawn(Agent {
            id: brain_agent_id,
            profile: AgentProfile {
                name: "brain".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["brain".to_string()],
                description: "brain agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: Default::default(),
            system_prompt: None,
        });

        // Spawn regular agent（带 "default" tag，会被 select_agent_for_sub_task_with_skill 选中）
        let regular_agent_id = Uuid::new_v4();
        app.world_mut().spawn(Agent {
            id: regular_agent_id,
            profile: AgentProfile {
                name: "default-llm-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["default".to_string()],
                description: "default agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: Default::default(),
            system_prompt: None,
        });

        // Spawn 一个带 SubTaskConfig 的 Task
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test-user".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input_ready("write some code", 0, channel);
        let task_id = task.id;
        let parent_agent_id = Uuid::new_v4();
        app.world_mut().spawn((
            task,
            SubTaskConfig {
                batch_id: Uuid::new_v4(),
                child_agent_name: "coder".to_string(),
                child_agent_model: None,
                allowed_tools: vec![],
                parent_agent_id,
                depends_on: vec![],
                depended_by: vec![],
            },
        ));

        // 运行一帧
        app.update();

        // 找到 task entity 并断言 TaskInjectedSkill 已注入且 skill_id 与预期匹配
        let task_entity = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &Task)>();
            query
                .iter(world)
                .find(|(_, t)| t.id == task_id)
                .map(|(e, _)| e)
                .expect("task should still exist")
        };

        let injected = app
            .world()
            .entity(task_entity)
            .get::<TaskInjectedSkill>()
            .expect("TaskInjectedSkill should be attached to task");
        assert_eq!(
            injected.skill_id,
            Some(SkillId::new("default-llm-agent", "coding")),
            "skill_id should match the skill registered to the selected agent"
        );
    }

    mod skill_selection_parse_tests {
        use super::*;

        #[test]
        fn parse_standard_json() {
            let json = r#"{"agent_name": "agent-a", "skill_name": "coding"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "agent-a");
            assert_eq!(skill, Some("coding".to_string()));
        }

        #[test]
        fn parse_null_skill_name() {
            let json = r#"{"agent_name": "agent-a", "skill_name": null}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_string_none_skill_name() {
            let json = r#"{"agent_name": "agent-a", "skill_name": "None"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_empty_string_skill_name() {
            let json = r#"{"agent_name": "agent-a", "skill_name": ""}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_extra_fields_ignored() {
            let json = r#"{"agent_name": "agent-a", "skill_name": "coding", "reason": "because"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
        }

        #[test]
        fn parse_invalid_json_errors() {
            let json = "not a json";
            let result = parse_brain_skill_selection(json);
            assert!(result.is_err());
        }

        #[test]
        fn parse_missing_agent_name_errors() {
            let json = r#"{"skill_name": "coding"}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_err());
        }

        #[test]
        fn parse_non_string_skill_name_returns_none() {
            // 数字类型 skill_name 应被容错为 None
            let json = r#"{"agent_name": "agent-a", "skill_name": 123}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "agent-a");
            assert_eq!(skill, None);

            // 布尔类型 skill_name 应被容错为 None
            let json = r#"{"agent_name": "agent-a", "skill_name": true}"#;
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (_, skill) = result.unwrap();
            assert_eq!(skill, None);
        }

        #[test]
        fn parse_strips_markdown_json_block() {
            // LLM 常见输出格式：```json ... ``` 代码块包裹
            let json = "```json\n{\"agent_name\":\"a\",\"skill_name\":\"coding\"}\n```";
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "a");
            assert_eq!(skill, Some("coding".to_string()));
        }

        #[test]
        fn parse_strips_bom_and_zero_width_chars() {
            // BOM (U+FEFF) + 零宽空格 (U+200B) 等不可见字符应被清洗
            let json = "\u{feff}\u{200b}{\"agent_name\":\"a\",\"skill_name\":\"coding\"}";
            let result = parse_brain_skill_selection(json);
            assert!(result.is_ok());
            let (agent, skill) = result.unwrap();
            assert_eq!(agent, "a");
            assert_eq!(skill, Some("coding".to_string()));
        }
    }
}
