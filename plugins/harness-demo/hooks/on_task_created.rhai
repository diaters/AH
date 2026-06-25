// on_task_created: 演示 WorldCommand（create_task）+ read_plugin_resource + entity query
let tasks = get_task_ids();
log_info(`[demo] on_task_created: ${tasks.len()} tasks now exist`);

// 演示 read_plugin_resource：读取插件目录内的文件
let greeting = read_plugin_resource("data/greeting.txt");
log_info(`[demo] greeting from plugin resource: ${greeting}`);

// 演示 create_task：创建一条追踪任务
let new_id = create_task("demo-plugin: tracking task created event");
log_info(`[demo] created tracking task: ${new_id}`);

// 演示 temp_resource：在当前 hook 执行期间暂存数据
register_temp_resource("last_event", "on_task_created");
