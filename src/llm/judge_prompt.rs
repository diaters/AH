//! Judge prompt 构建（LLM-as-Judge，设计文档 §6.2）。
//!
//! Judge 复用 `AgentRequestKind::Evaluation` 请求通道，通过 `system_prompt`
//! 注入 Judge 语境，不新增枚举变体。输出要求 JSON 格式的 `JudgeVerdict`，
//! 由 `parse_judge_verdict` 鲁棒解析。

use crate::domain::JudgeRubric;

/// Judge prompt 输入数据
#[derive(Debug, Clone)]
pub struct JudgePromptData<'a> {
    /// 场景名
    pub scenario_name: &'a str,
    /// 场景描述（评估背景）
    pub scenario_description: &'a str,
    /// 场景的用户输入
    pub user_input: &'a str,
    /// Agent 最终输出
    pub agent_output: &'a str,
    /// 工具调用摘要（每行一条，如 `shell_exec({"command":"ls"})`）
    pub tool_calls_summary: &'a [String],
    /// 评估规格
    pub rubric: &'a JudgeRubric,
    /// 附加评估说明（场景文件中的 rubric 自由文本）
    pub extra_instructions: Option<&'a str>,
}

/// Judge 系统 prompt：角色、维度与输出格式约束。
pub fn judge_system_prompt() -> String {
    r#"你是一个严格但公正的 AI 评估专家（Judge）。你的任务是评估另一个 AI Agent
在给定场景中的任务完成质量。

要求：
1. 仅基于给定的事实（用户输入、Agent 输出、工具调用摘要）作判断，不臆测未发生的行为
2. 每个维度独立打分（0.0-1.0），并给出具体理由
3. 保持判断稳定：相同输入应得到相同结论，不做发散性解读
4. confidence 表示你对本次判断的把握，不确定时如实调低

输出格式：仅输出一个 JSON 对象，不要附加任何其他文字，结构如下：
{
  "scores": [
    {"name": "<维度名>", "score": <0.0-1.0>, "rationale": "<评分理由>"}
  ],
  "pass": <综合裁决，各维度平均分不低于阈值时为 true>,
  "reasoning": "<综合裁决理由>",
  "confidence": <0.0-1.0>
}"#
    .to_string()
}

/// Judge 用户 prompt：场景事实 + 评估规格。
pub fn build_judge_user_prompt(data: &JudgePromptData) -> String {
    let dimensions = data.rubric.dimensions.join("、");
    let tool_lines = if data.tool_calls_summary.is_empty() {
        "（无工具调用）".to_string()
    } else {
        data.tool_calls_summary
            .iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let extra = data
        .extra_instructions
        .map(|text| format!("\n## 附加评估说明\n\n{text}\n"))
        .unwrap_or_default();

    format!(
        r#"## 评估场景

场景名：{scenario_name}
场景描述：{scenario_description}

## 用户输入

{user_input}

## Agent 最终输出

{agent_output}

## 工具调用摘要

{tool_lines}
{extra}
## 评估要求

评估维度：{dimensions}
通过阈值：各维度平均分不低于 {threshold}

请按系统指令输出 JSON 格式的评估结果。"#,
        scenario_name = data.scenario_name,
        scenario_description = data.scenario_description,
        user_input = data.user_input,
        agent_output = data.agent_output,
        tool_lines = tool_lines,
        extra = extra,
        dimensions = dimensions,
        threshold = data.rubric.threshold,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tool_summary() -> Vec<String> {
        vec!["shell_exec({\"command\":\"ls *.rs | wc -l\"})".to_string()]
    }

    fn sample_data<'a>(rubric: &'a JudgeRubric, tools: &'a [String]) -> JudgePromptData<'a> {
        JudgePromptData {
            scenario_name: "shell_stat_task",
            scenario_description: "统计当前目录下 .rs 文件数量并汇报",
            user_input: "统计当前目录下 .rs 文件数量并汇报",
            agent_output: "当前目录下共有 12 个 .rs 文件",
            tool_calls_summary: tools,
            rubric,
            extra_instructions: None,
        }
    }

    #[test]
    fn system_prompt_defines_json_output_contract() {
        let prompt = judge_system_prompt();
        assert!(prompt.contains("Judge"));
        assert!(prompt.contains("scores"));
        assert!(prompt.contains("pass"));
        assert!(prompt.contains("confidence"));
        assert!(prompt.contains("0.0-1.0"));
    }

    #[test]
    fn user_prompt_contains_scenario_facts() {
        let rubric = JudgeRubric::default();
        let tools = sample_tool_summary();
        let data = sample_data(&rubric, &tools);
        let prompt = build_judge_user_prompt(&data);

        assert!(prompt.contains("shell_stat_task"));
        assert!(prompt.contains("统计当前目录下 .rs 文件数量并汇报"));
        assert!(prompt.contains("12 个 .rs 文件"));
        assert!(prompt.contains("shell_exec"));
        assert!(prompt.contains("correctness"));
        assert!(prompt.contains("0.7"));
    }

    #[test]
    fn user_prompt_handles_empty_tool_calls() {
        let rubric = JudgeRubric::default();
        let data = JudgePromptData {
            scenario_name: "s",
            scenario_description: "d",
            user_input: "u",
            agent_output: "o",
            tool_calls_summary: &[],
            rubric: &rubric,
            extra_instructions: None,
        };
        let prompt = build_judge_user_prompt(&data);
        assert!(prompt.contains("（无工具调用）"));
    }

    #[test]
    fn user_prompt_includes_extra_instructions_when_present() {
        let rubric = JudgeRubric::default();
        let tools = sample_tool_summary();
        let mut data = sample_data(&rubric, &tools);
        data.extra_instructions = Some("数字应在 1-500 之间");
        let prompt = build_judge_user_prompt(&data);
        assert!(prompt.contains("## 附加评估说明"));
        assert!(prompt.contains("数字应在 1-500 之间"));
    }

    #[test]
    fn prompt_composes_into_evaluation_request_fields() {
        // Judge 请求通道复用 AgentRequestKind::Evaluation：system + user 两段
        // 拼装后应可直接作为 AgentExecutionRequest 的 system_prompt/prompt。
        let rubric = JudgeRubric::default();
        let tools = sample_tool_summary();
        let data = sample_data(&rubric, &tools);
        let system = judge_system_prompt();
        let user = build_judge_user_prompt(&data);
        assert!(!system.is_empty());
        assert!(user.starts_with("## 评估场景"));
    }
}
