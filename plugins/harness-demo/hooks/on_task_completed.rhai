// on_task_completed: 演示 get_task
let ids = get_task_ids();
log_info(`[demo] on_task_completed: ${ids.len()} tasks`);

if ids.len() > 0 {
    let t = get_task(ids[0]);
    log_info(`[demo] first task status: ${t.status}`);
}
