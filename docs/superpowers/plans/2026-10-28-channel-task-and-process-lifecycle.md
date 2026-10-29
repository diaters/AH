# 单通道任务与监控生命周期实现计划

> __面向 AI 代理的工作者：__ 使用 subagent-driven-development 按任务实施，先规格审查再代码质量审查。

__目标：__ 落实已确认的单通道用户任务约束、进程回收保障与持续监控通知。

__架构：__ 保留 ECS Task、TaskInbox 和既有注入链路；backend 统一管理进程，独立守护器兜底实例死亡。
监控输出经过现有通道投递，并将发送结果送回原任务。

__技术栈：__ Rust、Bevy ECS、tokio、现有 libc 与 serde_json；不增加第三方 crate。

__规格：__ [评审通过的修正规格](../specs/2026-10-28-channel-task-and-process-lifecycle-design.md)。

## 实施进度（2026-10-29）

- A/B/C：已分别通过 PR #114、#115、#116 合入主线；macOS 针对性与 Linux CI 验证通过，Android 设备待验收。
- D：已接通输出事件、平台回执、重试、背压与窗口唤醒，最终验证按本次交付记录执行。
- E：真实 provider 与 QQ 场景另列验收结果；下方复选框保留原始步骤，进度以本节与 PR 为准。

## 1. 文件边界

| 部分 | 实现文件 | 测试文件 |
| --- | --- | --- |
| 进程所有权 | `src/systems/tools/backend/native.rs` | native 单元测试、`tests/shell_exec_async_test.rs` |
| 工具归属 | `src/domain/tool_async.rs`、`src/systems/tools/builtin/shell/exec.rs` | shell 异步测试 |
| 独立守护器 | backend 下新模块、`src/main.rs`、应用关闭入口 | `tests/process_lifecycle.rs` |
| 用户任务唯一性 | routing、command、task_creation、ingress、插件创建入口 | `tests/multi_turn_routing.rs`、loop 测试 |
| 持续监控 | backend 输出读取、`domain/session.rs`、loop_control、通道 manager 与 traits | 监控集成测试、通道发送测试 |
| 文档 | README、current-state、configuration、`.env.example`、规格及计划索引 | markdownlint、引用核对 |

每个部分独立提交、分支与 PR；后续分支基于前序提交，评审时可只查看增量。

## 2. 任务 A：进程所有权与一次性回收

- [ ] 在 native 单元测试中添加 clone 不应杀共享会话的断言：

```rust
let backend = NativeProcessBackend::default();
let session = backend.start_session(exec_request("exec sleep 60", None)).unwrap();
drop(backend.clone());
let state = backend.read_session(SessionReadRequest {
    handle_id: session.handle_id,
    tail_lines: 20,
}).unwrap();
assert_eq!(state.status, SessionStatus::Running);
backend.stop_session(session.handle_id).unwrap();
```

- [ ] 为 `nohup sleep 60 >/dev/null 2>&1 & echo $!` 添加一次性命令返回后无存活后代的测试。
- [ ] 运行 `cargo test --lib systems::tools::backend::native::tests`，确认新断言因已复现行为失败。
- [ ] 将清理责任收敛到共享所有者，外层 clone 释放无副作用；实际执行登记真实 Task 归属。
- [ ] 在正常返回、取消、超时路径统一清理进程组、读取输出与回收句柄。
- [ ] 重跑 native 与 shell 异步测试，确认旧测试没有以“保留后台孙进程”为成功前提。
- [ ] 提交 `fix: 统一 shell 进程所有权与一次性执行回收`，进行规格及质量审查。

## 3. 任务 B：实例级进程守护与关闭

- [ ] 新建进程生命周期集成测试，通过构建出的 Harness 可执行文件调用内部守护入口。
  宿主启动受管 `sleep` 并写出进程身份，然后测试以 SIGKILL 终止宿主：

```rust
host.kill().unwrap();
host.wait().unwrap();
assert!(wait_until_process_stops(session_pid, Duration::from_secs(5)));
```

- [ ] 测试辅助函数只负责等待和身份校验，失败前必须清理诊断进程，避免测试自身留下孤儿。
- [ ] 确认强杀测试在无守护器的生产路径上失败。
- [ ] 实现一次实例一个守护器、启动握手、存活连接、进程组登记、停止与退出状态协议。
- [ ] 内部入口先于 provider、dotenv、日志和 TUI 初始化处理；普通工具命令不继承存活连接。
- [ ] backend 的启动、会话输入、输出与取消经守护器执行；启动失败不得回退为无人管理执行。
- [ ] 主程序正常关闭和错误退出显式停止 backend，再关闭通道与异步运行时；关闭幂等。
- [ ] 覆盖正常关闭、panic/abort、强杀、启动与关闭竞争、多个会话及独立 Harness 实例。
- [ ] 运行 `cargo test --test process_lifecycle`、native、shell 工具与 headless 测试。
- [ ] 提交 `feat: 为受管进程增加 Harness 死亡收尾保障`，进行两阶段审查。

## 4. 任务 C：单通道单用户任务

- [ ] 在现有集成夹具中同帧注入十条同通道输入，验证只有一个顶层用户 Task，且输入全部保留。
- [ ] 添加运行中 steer、等待用户续轮、审批和 ask_user 保留原语义的测试。
- [ ] 添加内部子任务并行、定时任务仅输出、其他通道可独立创建任务的测试。
- [ ] 添加 `/btw` 拒绝并行创建、`/stop` 保留名额、`/finish` 清理后新建的测试。
- [ ] 首次执行测试确认失败，再修改创建入口与路由。
- [ ] 创建入口按完整 ChannelId 和输入序号处理批次；同帧保留名额，避免 deferred 竞态。
- [ ] 命令与插件用户任务创建统一收口，普通输入只解析唯一用户任务，取消焦点与更新时间选择。
- [ ] 关闭期间保留后续输入，资源回收确认后再新建；异常多任务状态显式提示。
- [ ] 覆盖结束与新输入同帧、失败重试、终态保留、跨通道及群聊审批隔离。
- [ ] 运行 `cargo test --test multi_turn_routing --test loop_steering_flow --test cross_channel_isolation`。
- [ ] 提交 `feat: 保证每个通道只有一个活跃用户顶层任务`，进行两阶段审查。

## 5. 任务 D：持续事件、发送回执与自动处理

- [ ] 用受管脚本在保持存活时输出告警，断言用户通道收到事件且原 Task 收件箱收到通知。
- [ ] 验证默认不开启输出通知、stderr 不转发、notify_on_exit 兼容、无输出时静默。
- [ ] 验证连续第六与第十条事件可投递、合并截止固定、超长单行和 1 MiB 背压有明确结果。
- [ ] 为通道失败、三次重试、最终发送失败及结束后的迟到回执添加测试。
- [ ] 确认新行为测试失败，再增加 `notify_on_output`、内部有界事件和发送结果关联。
- [ ] 事件由 Harness 先通过任务输出通道发送，Agent 获得排队及最终送达结果；不重复原样发送。
- [ ] `Waiting(BackgroundJobs)` 允许消费输出与 steer；自然完成仍等待所属后台工作结束。
- [ ] 自动唤醒采用十秒五轮滚动窗口；超限自动恢复，用户 steer 不受此额度约束。
- [ ] 空闲结束检查真实受管会话、在飞执行、待处理通知和未完成子任务。
- [ ] 构建日志记录提交身份；任务结束停止新事件，旧回执不能唤醒新任务。
- [ ] 运行监控、loop、通道 manager 与空闲回收测试，提交 `feat: 接通持续监控事件与通道送达回执`。
- [ ] 对完整通知链路进行规格与质量审查。

## 6. 任务 E：文档、全量验证与交付

- [ ] 按真实实现更新 README、current-state、configuration、`.env.example` 和工具帮助。
- [ ] 同步 loop 与无人值守监控设计的状态；规格标注已实施部分，保留平台与外部条件限制。
- [ ] 运行 `markdownlint`、`cargo fmt --all --check`、模块依赖和状态赋值断言。
- [ ] 运行 `cargo clippy --all-targets --all-features -- -D warnings` 和 `cargo test --all-features`。
- [ ] 真实 QQ 场景只使用用户已授权的环境；没有实际发送验证时明确记录未验证，不能以 mock 代替结论。
- [ ] 自审所有 diff 与测试证据，修复审查发现，再推送分支并创建关联的 draft PR。
- [ ] 附加全部 PR 到本聊天；最终说明修改范围、验证结果和平台限制，不自动合并。

## 7. 规格覆盖自检

- 规格 3 节由任务 C 覆盖；结束释放门槛依赖任务 A、B 的清理结果。
- 规格 4、5 节由任务 A、B 覆盖，包括临时 clone、已返回 exec 和强杀三处失败用例。
- 规格 6、7 节由任务 D 覆盖；平台真实送达与延迟证据在任务 E 中单独报告。
- 规格 8、9 节的顺序、文档同步和验收矩阵贯穿 A 至 E。
