// query_tasks 工具：查询当前 World 中的任务快照
let ids = get_task_ids();
let count = ids.len();

// 构建结果摘要
let summary = `Tasks in world: ${count}`;

// 逐个查询任务详情（最多前 5 个，避免过长输出）
let details = "";
let limit = 5;
if count < limit {
    limit = count;
}

let i = 0;
while i < limit {
    let t = get_task(ids[i]);
    details = details + `\n  - ${t.id} [${t.status}]: ${t.content}`;
    i = i + 1;
}

if count > limit {
    details = details + `\n  ... and ${count - limit} more`;
}

summary + details
