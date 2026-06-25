// /demo-status 命令：返回当前 World 快照摘要
let tasks = get_task_ids();
let agents = get_agent_ids();
let skills = list_skills();

`[harness-demo] World Snapshot:
  Tasks:   ${tasks.len()}
  Agents:  ${agents.len()}
  Skills:  ${skills.len()}`
