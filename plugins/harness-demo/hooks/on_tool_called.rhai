// on_tool_called: 前置 hook — 演示 tool_deny + entity query
let tasks = get_task_ids();
log_info(`[demo] on_tool_called: ${tasks.len()} tasks in world`);

// 演示条件拒绝：对名为 "demo:block" 的工具调用执行 tool_deny
// 正常使用时不会触发，仅当 LLM 尝试调用此不存在的工具时演示拒绝能力
// 注：hook 系统无法获取当前 tool_name，因此这里仅做无条件观察
// tool_deny 的实际效果在 on_tool_called 的集成测试中已验证

// 演示 list_skills：查询当前注册的技能
let skills = list_skills();
log_info(`[demo] ${skills.len()} skills registered`);
