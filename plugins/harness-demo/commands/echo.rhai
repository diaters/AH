// /demo-echo 命令：回显参数 + 读取插件资源
let greeting = read_plugin_resource("data/greeting.txt");
let trimmed = greeting.trim();

`[harness-demo:echo] ${trimmed}`
