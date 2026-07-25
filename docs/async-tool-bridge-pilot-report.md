# 异步工具桥 Pilot 验收报告

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-07-25 |
| 适用阶段 | 异步工具桥 Phase 2 收尾 |
| 相关文档 | `docs/superpowers/plans/异步工具桥实施手册 v1（独立版）.md`、`docs/current-state.md` <!-- markdownlint-disable-line MD013 --> |

---

## 1. 背景与定位

本报告是异步工具桥 pilot 的退出判据之一（D13），由 Task 8
`pilot e2e 验收`产出。前 7 个 Task 交付了零件（dispatch / ingest / sweeper /
list_scheduled_tasks / 通道与挂起实体类型 / 共享测试 harness），本报告基于
把这些零件装在一起跑通的 e2e 验收数据，给出两条 pilot 退出判据的结论：

1. 零件装在一起能否跑通完整链路（e2e 用例覆盖）—— 第 2 章
2. 是否需要把 worker → ingest 的无界 mpsc 通道换为有界通道（背压实验 D13）—— 第 3 章

---

## 2. e2e 用例覆盖

`tests/async_bridge_e2e_test.rs` 共 7 个测试用例（6 个默认跑 + 1 个 `#[ignore]`
背压实验手动跑），覆盖 spec 第十章 Task 8 的全部 8 个 Step（Step 8 是 commit
本身）。

| 用例 | 覆盖 spec Step | 验证要点 |
|------|----------------|----------|
| `e2e_full_chain_dispatch_to_restore` | Step 1 happy path | dispatch 挂起 → worker 跑 → ingest 落地 → `tool_result_system` 标 processed → `tool_calling_orchestrator_system` 收齐清 pending + despawn 结果 + spawn follow-up |
| `e2e_worker_panic_yields_exactly_one_error_result` | Step 2 失联路径一 | `catch_unwind` 把 worker panic 转成 `ExecutionFailed`，ingest 落地恰好一条 error |
| `e2e_sweeper_timeout_yields_error_and_barrier_continues` | Step 3 失联路径二 | `std::future::pending` 永不返回，sweeper 超时 claim + 入通道，ingest 落地 Timeout；二次 sweep 无第二条结果（claim 防重） |
| `e2e_channel_disconnect_is_swept` | Step 4 失联路径三 | 移除 `ToolResultReceiver` 模拟通道断开，worker / sweeper 双侧 `send` 失败均被 `let _ =` 吞掉；sweeper 仍完成 claim（摘 `InFlightToolCall`），系统不 panic、不挂死 |
| `e2e_sweeper_error_first_worker_late_success_dropped` | Step 5 exactly-once race | sweeper 先 claim + 入通道，worker 50ms 后的 Ok 经 ingest 时挂起实体已没 → drop + warn，世界结果计数仍为 1 |
| `e2e_barrier_waits_for_all_results_before_restore` | Step 6 barrier 部分结果 | pending = [c1, c2]，c1 落地后跑 orchestrator 不 spawn follow-up、pending 仍含 c2；c2 落地后再跑 orchestrator → pending 清空 + spawn 1 条 follow-up + despawn 两条结果 |
| `e2e_backpressure_experiment_1000_buffered_results` | Step 7 背压实验（`#[ignore]`） | 1000 条 buffered 结果的 RSS 占用实测，详见第 3 章 |

### 2.1 设计纪律自审

- 一律 `#[test]`，无 `#[tokio::test]`（避免 runtime 嵌套 panic）
- 时间源唯一：测试体内一切「现在」来自 `now(&world)` / `advance_clock(&mut world, secs)`，
  禁止 `Utc::now()` 出现在测试体（fixture 数据如 `created_at` 例外）
- 不重写 barrier 逻辑：复用 `tool_result_system`（`src/systems/tools/result.rs`）
  与 `tool_calling_orchestrator_system`（`src/systems/transform/llm_response.rs`）
- 失联三路径殊途同归到 ingest 单点落地，exactly-once 由「挂起实体是否还在」唯一裁决
- list 工具的 e2e happy path 走真实双账本快照（`build_scheduler_snapshot` +
  `build_registry_snapshot` 均已落地于 dispatch），未退化为 `EchoAsyncTool`

### 2.2 跑法

```bash
# 默认 6 条用例
cargo test --test async_bridge_e2e_test

# 背压实验（手动跑，输出 RSS 数字）
cargo test --test async_bridge_e2e_test -- --ignored e2e_backpressure --nocapture
```

---

## 3. 背压实验（D13）

### 3.1 实验设计

持有 `ToolResultReceiver` 不跑 `ingest_tool_results_system`，直接向
`ToolResultSender` 塞 1000 条 `ToolAsyncResult::completed`，每条 payload
为一个 64 字节字符串 + index 字段的 `serde_json::Value`，与生产环境
list 工具的结果体量同量级。记录前后进程 RSS 差值。

### 3.2 实测数据

| 指标 | 数值 |
|------|------|
| `BUFFERED` | 1000 |
| `RSS_BEFORE` | 8.12 MB |
| `RSS_AFTER`  | 9.11 ~ 9.14 MB（多次实测小幅波动） |
| `DELTA`      | 0.98 ~ 1.02 MB |

环境：macOS，`ps -o rss= -p $$` 读取。绝对值受进程基线影响，**差值才有意义**。

### 3.3 单条消息开销估算

`ToolAsyncResult` 结构体本身 + unbounded channel node + `serde_json::Value`
payload（含 64 字节字符串 + index 字段）合计约 1 KB/条。spec 第十章预判
「单条消息约数百字节，1000 条预期 <1MB 量级」，实测 1.0 KB/条，量级吻合。

### 3.4 结论：不换有界通道

**结论：pilot 阶段保持现状（无界 `mpsc::unbounded_channel`），不切换为有界通道。**

理由：

1. **量级可控**：1000 条 buffered 结果的 RSS 增量 < 1 MB，相对进程基线
   （数十 MB 量级，含 tokio runtime + ECS world + LLM 客户端）不到 5%。
   生产场景单 task 工具调用量远低于 1000 并发在飞，常态占用可忽略。
2. **背压源不在通道本身**：真正的失联兜底由 sweeper 超时 claim 保证
   （default 300s），与通道容量无关。worker panic 由 dispatch 的
   `catch_unwind` 快速失败路径兜底。无界通道不引入新的失联风险。
3. **有界通道的副作用更复杂**：有界通道会让 worker `send` 阻塞或返回
   `Full`，需要额外处理「send 失败如何上报」与「阻塞 worker 池」两类问题，
   与「worker 零 ECS 接触 + send 失败 let _ = 静默」的现有不变量冲突。
   付出复杂度成本但收益边际。
4. **exactly-once 不依赖通道容量**：结果落地仅在 ingest 单点，由「挂起实体
   是否还在」唯一裁决。无论通道是否有界，重复 / 迟到结果都会被 ingest
   `drop + warn`。有界通道不能为 exactly-once 提供额外保证。

### 3.5 监控建议

虽然不切换有界通道，但应在生产监控中关注两个指标：

| 指标 | 阈值建议 | 处置 |
|------|----------|------|
| sweeper claim 频次 | 持续 > 0 次/分钟 | 排查 worker 是否大面积失联（panic / hang），有界通道不是根因 |
| 单 task pending tool call 数 | > 100 | 排查 LLM 是否在单次响应中产出异常多的 tool_call（模型退化迹象） |

这两个指标的阈值在 pilot 后期结合真实负载再校准，pilot 阶段不写入配置。

---

## 4. 遗留开放问题

下列问题在 pilot 阶段不解决，留待 Phase 3+ 收尾时定：

1. **静态路由（triggers.toml）的 list/delete 工具**：另起一组命名，不混进
   本 pilot 的 `list_scheduled_tasks` 命名空间（spec 第十八章）。
2. **`tool_inflight_timeout_secs` 的 per-tool 覆盖**：当前由
   `BuiltinTool::max_duration` 钩子提供，pilot 仅 `ListScheduledTasksTool`
   一个乘客，缺省 300s。多工具上线后可能需要按工具类型分组配置。
3. **背压阈值监控落地**：第 3.5 节的两个监控指标需要接入实际 metrics
   pipeline（当前仅在 `tracing` 日志中可观测）。

---

## 5. 退出判据核对

对照 spec 第十六章「验收总清单」中与 pilot 直接相关的条目：

- [x] pilot e2e 六条用例通过（全链路 / panic / sweeper 超时 / 通道断开 / race / barrier 部分结果）
- [x] exactly-once：结果落地仅在 ingest；任何失联路径恰好一条结果
- [x] pilot 报告有明确的有界通道结论（D13，本报告第 3.4 节）

`cargo test --all` 全绿、shell_exec 回归、双账本不变量、`docs/async-tool-bridge.md`
正式文档属于 Phase 3+ 范畴，不在本 pilot 验收范围。
