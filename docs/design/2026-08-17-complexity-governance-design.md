# 设计质量治理计划：从时间顺序分解走向知识域分解

> __状态：当前有效__ — 2026-08-17 完成设计评审并修订（N1/N2/S1-S4/C1-C4 全部采纳，
> N2 选方案 A、S3 选 contracts 归属）。P0–P5 全部阶段已实施完成，实施记录见 §8。
> 评审报告见 `logs/review/2026-08-17-complexity-governance-design-review.md`。
> 产出背景：2026-08-17 基于《软件设计的哲学》（A Philosophy of Software Design）核心观点的全仓库质量审视。

路径约定：文中 `systems/...`、`domain/...` 等简写均以 `src/` 为根（如
`transform/llm_response.rs` 即 `src/systems/transform/llm_response.rs`）。

## 1. 背景与问题清单

审视结论：项目呈「局部深、全局浅」状态。`llm/`、memory 持久化、QQ/Telegram 协议封装、
`user_plugins/` 沙箱是深模块典范；但系统级组织存在以下结构性问题（按严重度排序）：

| # | 问题 | 核心证据 | 违反的原则 |
| --- | --- | --- | --- |
| Q1 | 时间顺序分解：结果处理按执行阶段而非知识域归属 | `transform/llm_response.rs` 1908 行，集中评估/摘要/画像等结果规则 | 按知识分解 |
| Q2 | 拆分维度失衡：微文件与巨函数并存 | `handle_tool_action` 1206 行、18 参数（`orchestrator.rs:356`）；`shell/list.rs` 仅 17 行 | 不为拆分而拆分 |
| Q3 | 依赖环：domain 反向依赖 5 个上层模块，另 3 组环 + 1 处违规 | domain 借用 `SkillId` 等；app↔systems；contracts→ecs；glob 掩盖依赖来源 | 分层单向依赖 |
| Q4 | 登记性知识泄露 | `FrontendKind` ↔ 通道名映射 6 处（含 2 处 `panic!`）；`qq.rs:644` 反向写配置 | 信息隐藏 |
| Q5 | 信息隐藏被绕过 | `Task` 21 字段全 `pub` 且有绕过点；`WorkItem` 无任何转换方法 | 用类型消灭错误 |
| Q6 | 接口比实现宽 | `WorldCommand` 未实现仅跳过；contracts 9 trait 仅 2 个有消费者 | 语义诚实 |

## 2. 治理原则

- __行为不变优先__：重组类改动一律"纯搬家"，函数体不改逻辑，由现有测试守门。
- __投资思维__：每个 PR 都让结构严格优于改动前；不追求一次到位的完美目录。
- __精力投放__（书中第十六章）：先动复杂度最高、被依赖最广、变化最频繁处；llm 接入与
  QQ/Telegram 协议实现已被验证为深模块，本轮不动。
- __规范闸门__：每个阶段独立分支 + PR + CI（fmt / clippy / test），遵循 GitHub Flow。

## 3. 阶段计划

阶段间依赖：P0 → P2 → P3 顺序执行；P1、P4、P5 相对独立，可穿插。

### P0：模块依赖方向治理

__目标__：消灭全部 8 组依赖环并治理 1 处分层违规，形成单向分层（下层不允许引用上层，
同层括号内不允许相互引用）：

```text
domain ← contracts ← {ecs, llm, channels, tui, infrastructure}
       ← {user_plugins, triggers} ← systems ← {plugins, app}
```

__决策准则__（三种情形）：

1. 被 domain 需要的稳定领域概念 → 下沉到 `domain/`；
2. 技术细节 → domain 侧改用中性类型；
3. 被 systems 需要的运行时资源抽象（非领域概念）→ 下沉到 `contracts/`
   （"契约"层恰位于 domain 与 systems 之间，保持 domain 语义纯度）。

任务清单：

- 下沉以下类型到 `domain/`（原位置 `pub use` 过渡或直接改引用）：
  - `SkillId`（现 `infrastructure/skills/registry.rs`）
  - `HookPoint`（现 `user_plugins/hook_point.rs`；`domain/contribution.rs` 等需要它）
  - `LlmProviderKind`（现 `llm/`；`domain/model_chain.rs` 引用）
  - `ScheduleSpec`（现 `triggers/scheduled_task.rs`）
  - `SessionBackend`（现 `contracts/`；`domain/tool_async.rs` 引用）
- 解 app↔systems：`app::Clock`（app/mod.rs:246）、`app::FrontendRegistry`（app/mod.rs:194）
  下沉到 `contracts/`——二者是被 systems 需要的运行时资源抽象而非领域概念
  （`Frontend` trait 已在 `domain/frontend.rs`，下沉不引入新依赖）。
- contracts 死抽象裁决（先于 `Clock`/`FrontendRegistry` 下沉执行）：9 个 trait 中仅
  `MemoryStore`、`SessionBackend` 有真实消费者，其余 7 个（`ToolCatalog`、
  `ToolApprovalPolicy`、`ExecutionBackend`、`ExecutionPolicy`、`MemoryCompactor`、
  `ContributionPolicy`、`BrainSelectionPolicy`）零或近零消费者——逐个裁决删除或激活，
  默认删除（AGENTS.md 代码腐化治理：禁止保留脱节抽象）。
- 解 contracts→ecs 分层违规：随死抽象裁决优先直接删除 `ToolCatalog`（零消费者，删除后
  `contracts/tools.rs:8` 对 `ecs::EntityIndex` 的违规引用自然消除）；若裁决为激活，再按
  "`EntityIndex` 下沉 contracts 或改泛型接口"处置。
- 解 triggers↔app：`triggers/mod.rs:31` 引用 `app::HarnessSettings`，将触发器所需配置拆为
  domain 配置类型或改为启动注入。
- 解 systems↔user_plugins：`user_plugins/integrate.rs` 调用
  `systems::tools::register_plugin_tools` 的方向反转——systems 装配时主动从 `user_plugins`
  registry 拉取并注册。
- 保留的合法单向边（同层规则澄清后无需处理）：systems→user_plugins（hook 系统）、
  systems→triggers（`effect_commit.rs:16-17` 等 5 处）、infrastructure→contracts。
- 移除 `lib.rs` 七连 glob re-export（`pub use app::*` 等，lib.rs:14-20）：统一为全路径
  引用或单点精确 re-export，保证依赖方向断言的可靠性（glob 使类型来源不可见）。波及
  `tests/` 大量引用点，机械替换由编译器驱动。

__验收__：

- domain 内部依赖断言（白名单形式，豁免 bevy 重导出与域内自引用）：

  ```bash
  # domain 只允许引用 crate::prelude（bevy 重导出）与 crate::domain 自身
  ! grep -rn "use crate::" src/domain/ \
    | grep -vE "crate::(prelude|domain)(::|\b)" | grep -q .
  ```

- CI 检查脚本按上述目标分层固化模块依赖方向断言（同层与反向引用即失败）。
- `lib.rs` 不再包含模块级 glob re-export（`pub use xxx::*`）。
- `cargo test --all-features` 全绿。

__风险与回滚__：纯类型搬移 + 引用改写，编译器全程守门；单 PR 可 revert。

### P1：登记性知识收口

__目标__：消灭 shotgun surgery 点与运行时 `panic!`。新增 IM 通道的改动面清单（现状）：
登记性散落点 8-9 处（6 处 kind↔name 映射、2 处 schema 硬编码枚举、1 处 TUI 标签）+
必然装配点 3 处（通道实现文件、`channels/config.rs` 结构体与 `channels/mod.rs` 注册、
`main.rs` 装配）。目标：登记性散落点清零，新增通道收敛为"枚举变体 + 实现文件 + 配置
注册"三处最小集合。

任务清单：

- `FrontendKind` 成为唯一权威：在 `domain/frontend.rs` 提供 `channel_name()` 与
  `from_channel_name()`，替换全部 6 处映射（`channels/traits.rs:42`、
  `channels/manager.rs:13`、`systems/frontend_output.rs:215`、
  `systems/tools/builtin/schedule_task.rs:124`、`tui/status.rs:29` 等）。
- 消灭 panic：上述 2 处 `panic!("unknown channel name")` 改为启动期配置校验错误。
- 工具 schema 通道枚举生成化：`channels/send_tool.rs:31`、`systems/tools/mod.rs:404` 的硬编码
  `"enum": [...]` 由 `FrontendKind` 变体生成。
- 移除未实现的 `FrontendKind::Feishu` 变体与空的 `channels/lark.rs`（名字先于实现扩散；
  实现时随真实通道一并加回）。
- `agents.toml` 解析收口（两步）：
  1. 从 `infrastructure/incubation/agent_registry.rs` 提取公共只读接口
     `load_agent_config(path) -> Result<AgentConfig>`（消除其 3 个写方法内嵌的重复解析）；
  2. `systems/maintenance.rs:86-109` 改调该接口。失败语义显式声明：现状两侧相反
     （registry 容错 warn 吞掉 vs maintenance 解析失败 `panic!`），统一为启动期配置校验
     错误（与"消灭 panic"基调对齐），文件缺失保持 warn 容错不变。
- 审批 `callback_data`（`<request_id>:<option_id>`）格式收口：生成与解析集中到单一模块，
  `channels/frontend.rs:240`、`qq.rs:277-309`、`telegram.rs:1441-1448` 三处调用之。
- skill 布局知识收口：`infrastructure/skills` 暴露读写接口，消除
  `systems/experience/skill_update.rs:195,641,658`、`skill_creation.rs:41,299,362` 的直接
  `std::fs` 调用。
- QQ 通道配置写入收口：`qq.rs:644-666` `persist_allowed_user` 直接 `tokio::fs::write`
  TOML 并知晓 `ChannelConfigs` 全结构，改为经 infrastructure 提供的配置写入接口
  （文件 IO 知识退出通道模块，与 skill 布局收口同类）。
- `[IMAGE:path]` marker 语法说明文本定义为常量单点维护（解析已收口在
  `channels/traits.rs:131`）。

__验收__：

- 全仓库 grep 通道名字符串映射仅剩 `domain/frontend.rs` 一处定义。
- 通道相关集成测试全绿；新增一条"未知通道名报配置错误而非 panic"的测试。

### P2：WorkItem 结果处理按知识域重组

__目标__：每个领域（评估、摘要、画像、brain）拥有"触发 → 结果处理"完整知识；
`llm_response.rs` 回归纯路由 + 通用 LLM 响应处理。

__现状构成__（1908 行）：三个领域结果函数约 550 行 + 路由主体 `llm_response_system`
约 838 行（L720-1557）+ 工具调用编排 `tool_calling_orchestrator_system` 约 351 行
（L1558 起）+ 辅助函数与文件头约 169 行。

任务清单：

- `handle_evaluation_work_item_result`（`llm_response.rs:214-506`）迁至
  `systems/evaluation.rs`。
- `handle_summarization_work_item_result`（`llm_response.rs:507-666`）迁至 summarization
  领域模块；将其与 `memory_compression_system` 共用的"配对组选择逻辑"
  （`llm_response.rs:536-539` 注释自认）收敛到 `domain::memory` 单一权威出处。
- `handle_profile_generation_invalid`（`llm_response.rs:117-213`）迁至
  `systems/experience/profile_generation.rs`。
- `tool_calling_orchestrator_system`（L1558 起）迁至 `systems/tools/`——它是工具调用编排
  知识，本属 P3 邻域，随本阶段先行归位。
- 路由主体领域分支归位：`llm_response_system` 内各 WorkItem kind 的内联结果构造与任务
  状态回写分支（约 400+ 行）随对应领域迁出，路由主体只保留分派。
- brain 决策知识归位：明确 `parse_brain_skill_selection` 的归属与可见性，消除
  `transform/brain_decision.rs:25` 对 dispatch 内部函数的跨目录引用；将
  `dispatch/brain_dispatch.rs`、`brain_llm_builder.rs`、`transform/brain_decision.rs` 合并为
  单一 brain 知识域（已决策：合并）。
- 清理 `brain_dispatch.rs:3-6` 自认的"派发架构重组残留物"注释。

__验收__：

- 搬迁 diff 以 move 为主（`git diff --find-copies=40%` 可识别），函数体无逻辑改动。
- `llm_response.rs` 降至 500 行以内；各领域模块同时包含触发与结果处理两侧。
- brain 决策相关函数集中在单一目录；`transform/` 不再引用 `dispatch/` 内部函数。
- 现有集成测试全部通过。

### P3：拆解 `handle_tool_action`

__目标__：消灭 1206 行 / 18 参数的巨函数，同时不引入"参数对象式透传"。

任务清单：

- 18 个参数按语义分组为上下文结构体（World/Query 检索、资源句柄、时钟与配置），分组必须有
  真实语义，不做机械打包。
- 按工具类别与执行路径（sync / async / shell / scheduled / plugin）拆出各自处理函数，主函数
  只保留分派。
- 将 `InFlightToolCall` 的 claim 语义、"落地 + despawn 仅发生在 ingest"等流水线协议约定，
  从散落注释集中为模块级 rustdoc 单一权威。
- 微文件合并顺带处理：`tools/builtin/shell/` 下 17-41 行的单工具文件评估合并为
  `shell_tools.rs`（拆分粒度双峰的另一端）。

__验收__：任一函数不超过 200 行；工具链路既有测试全绿；协议约定有单一出处。

### P4：领域类型收紧

__目标__：用编译器替代"荣誉制度"，非法状态与绕过路径在类型层面不可表达。

任务清单：

- ID newtype 化：`pub type TaskId = Uuid` 等别名（`domain/mod.rs:36-37`、
  `domain/session.rs:10`）改为 newtype struct，编译器驱动全量替换，杜绝 ID 互传。
- 消灭状态绕过：`systems/maintenance.rs:515-519`、`transform/llm_response.rs:1675-1681`
  的直接三字段赋值改走 `Task::mark_failed`（恢复结构化 `TaskStatusTransition` 日志）。
- `Task` 的 `status`/`last_error` 等状态字段可见性收窄至 `pub(crate)`，转换一律经
  `mark_*` 方法。
- `TaskRoutingPolicy` 字段私有化，合法组合仅经 `conversational`/`event`/`scheduled_task`
  工厂构造。
- `WorkItem` 对齐 Task 模式：现 `status`/`assigned_agent` 全 `pub` 且无任何转换方法，
  补充 `mark_*` 状态转换方法并收窄字段可见性。

__验收__：仓库内不存在对 `task.status` 的直接赋值（grep 断言入 CI）；ID 混用产生编译错误。

__风险__：波及面实测 62 个文件（`src/` 47 + `tests/` 15），必须独立分支、独立 PR，不与
其他阶段混排。

### P5：接口与实现对齐（清理）

任务清单：

- `WorldCommand` 未实现命令（`dispatcher.rs:263-272`）：从枚举删除（默认方案，语义诚实）；
  若确有近期需求则实现，二选一不留"跳过"状态。
- `llm/factory.rs:14-24` 四分支同构 match 简化为直接构造。
- 前置 hook 补齐 `tool_name`（条件拒绝是该 hook 的核心用途，当前残缺）。
- `HookPoint` 三重字符串映射（`FromStr`/`as_serialized`/测试清单）用宏或常量数组单点化。
- 修正 `HARNESS_PLUGINS_DIR` 默认值的文档与代码不一致
  （`docs/configuration.md:230` vs `user_plugins/loader.rs:13`），同时收敛 `mod.rs:35` 与
  `reload.rs:86` 重复的默认值读取逻辑为单点。
- `llm/registry.rs:24` `model: "placeholder"` 字段语义修正（模型名实际永远被请求
  override 覆盖，改为 `Option` 或显式文档化，实施时定）。

## 4. 不做什么

- 不重构 ECS 调度结构（`HarnessSet` 阶段化调度是 Bevy 的合理用法，问题只在文件组织）。
- 不动 `llm/` 接入层、QQ/Telegram 协议实现、`user_plugins/` 沙箱内部（审视确认的深模块，
  保持不动即最佳投资）。例外：`qq.rs` 的 `persist_allowed_user` 配置写入路径属文件 IO
  知识而非协议实现，由 P1 收口。
- 不引入新第三方依赖；依赖方向检查用脚本实现（P0）。
- 不做"大爆炸"式目录重命名；所有搬迁保持 git 可追溯的 move 语义。

## 5. 验证与回滚策略

- 每个 PR 通过 CI 全量检查（markdownlint / fmt / clippy `-D warnings` / test）。
- 搬家类改动以 `git diff --find-copies=40%` 自查，确保评审者看到的是 move 而非重写。
- 任一阶段失败单 PR revert，不影响已合入的其他阶段。
- P0 合入后立即固化依赖方向 CI 断言，防止环回退。

## 6. 与现有文档的关系

- 实施过程中若与 `docs/design/2026-07-18-dispatch-architecture-unification-design.md`
  （派发架构统一）或 `docs/design/2026-06-06-workitem-boundary-design.md`（WorkItem 边界）
  产生表述冲突，以实际代码为准并同步修订对应文档。
- 各阶段合入后，在本文件追加实施记录小节；全部完成后按文档生命周期归档，并将结构结论
  沉淀到 `docs/current-state.md`。

## 7. 修订记录

- 2026-08-17 初版（草案）。
- 2026-08-17 评审修订：采纳评审报告全部意见——N1（P0 验收 grep 断言改白名单，豁免
  `crate::prelude` 与自引用）、N2 方案 A（P2 扩充 `tool_calling_orchestrator_system` 迁移与
  路由主体领域分支归位，维持 500 行验收）、S1（目标分层细化并显式声明合法单向边；补
  contracts→ecs 处置）、S2（`agents.toml` 改两步：提取 `load_agent_config` 只读接口 +
  统一启动期校验语义）、S3（`Clock`/`FrontendRegistry` 下沉 `contracts/`，决策准则扩为
  三情形）、S4（数字勘误：1206 行/18 参数、21 字段、1908 行；通道改动面给出清单）、
  C1（状态改"当前有效"）、C2（P4 波及面改实测 62 文件）、C3（文首路径约定）、
  C4（P2 验收补 brain 判定标准）。用户决策：N2 选方案 A，S3 选 contracts 归属。
- 2026-08-17 覆盖度补充：纳入首轮审视中遗漏的发现——G1（contracts 7 个无消费者 trait
  裁决并入 P0，先清理再下沉；`ToolCatalog` 处置改为优先删除）、G2（QQ
  `persist_allowed_user` 配置写入收口入 P1，§4 豁免表述相应修订）、G3（`WorkItem`
  状态转换约束入 P4）、G4（`lib.rs` 七连 glob re-export 移除入 P0 并新增验收断言），
  另补 P5 两条小项（插件目录默认值逻辑去重、registry `placeholder` 字段语义修正）。
  用户决策：G1 并入 P0、G2 入 P1、G4 入 P0。

## 8. 实施记录

### P0：模块依赖方向治理（2026-08-17，待 PR 合并）

- 死抽象裁决：7 个无消费者 trait 全部删除（`ToolCatalog`、`ToolApprovalPolicy`、
  `ExecutionBackend`、`ExecutionPolicy`、`MemoryCompactor`、`ContributionPolicy`、
  `BrainSelectionPolicy` 及伴生类型）。其中 `BrainSelectionPolicy`/`FirstBrainPolicy`/
  `AgentCapabilitySummary` 在 `brain_llm_builder.rs` 有仪式性使用（构造摘要后取第一个
  候选），简化为直接迭代取首个后删除；`contracts/memory.rs` 精简为仅 `MemoryStore`。
- 类型下沉：`SkillId`、`HookPoint`、`LlmProviderKind`、`ScheduleSpec`（含
  `compute_next_trigger`）、`SessionBackend` 按计划下沉 `domain/`。
- 实施中发现的隐藏反向边（内联路径，非 use 语句，原审视 grep 未覆盖）一并处置：
  `prompt_template.rs` 整体下沉 `domain/`（消灭 `domain/signal_trigger.rs` → triggers）；
  `ChannelAttachment`/`AttachmentKind`/`extract_attachments` 下沉 `domain/attachment.rs`
  （消灭 `domain/{message,space}.rs` → channels）。
- app↔systems 解环超出原任务清单的部分（同属 systems→app 反向边，全部清零）：
  通道资源桥 `InputReceiver`/`ExecutionResult{Sender,Receiver}`/
  `ModelChainStateUpdate{Sender,Receiver}` 归位 `domain/message.rs`（与
  `ToolResultSender` 同模式）；`ShutdownState` 归位 `domain/command.rs`；
  `MemoryConfig` 归位 `domain/memory.rs`；`HarnessConfig`/`HarnessSettings`/
  `BrainConfig` 归位 `systems/runtime_config.rs`（依赖 llm/channels，无法放
  contracts）；`ExecutorHandle`（自注释"临时向后兼容"零消费者）按死抽象删除。
- triggers↔app：新增 `domain::TriggersConfigPath` 投影注入，
  `reload_triggers_system` 改读该资源。
- systems↔user_plugins：`plugin_load_startup_system` 改 exclusive system 只加载
  registry 与技能贡献；工具注册经 `systems::tools::register_plugin_tools_in_world`
  由装配期主动拉取（Startup 排序 + reload 后置调用）。
- lib.rs 七连 glob 移除；tests（92 文件）与 main.rs 全量改模块全路径引用。
- 分层断言澄清（脚本注释固化）：plugins 为 systems 的装配扩展，允许
  plugins→systems 单向边；app 最顶层。`scripts/check_module_deps.sh` 已接入
  CI（code-check job 首步）。
- 验证：`cargo fmt` / `cargo clippy -D warnings` / `cargo test --all-features`
  （79 套件）全绿；断言脚本本地通过。

### P1：登记性知识收口（2026-08-19）

- `FrontendKind` 单一权威：`domain/frontend.rs` 提供 `channel_name()`/`from_channel_name()`，
  替换原 6 处散落映射；2 处 `panic!("unknown channel name")` 改为启动期配置校验错误
  （`grep` src/ 已无残留）。
- 移除未实现的 `FrontendKind::Feishu` 变体与空 `channels/lark.rs`（名字先于实现扩散；
  实现时随真实通道一并加回）。
- 工具 schema 通道枚举生成化：`systems/tools/mod.rs` 由硬编码改为
  `FrontendKind::ALL.iter().map(|k| k.channel_name())` 生成。
- `agents.toml` 解析收口：从 `infrastructure/incubation/agent_registry.rs` 提取
  `load_agent_config` 只读接口，`systems/maintenance.rs` 改调该接口，统一解析语义。
- 审批 `callback_data` 格式收口：生成 `make_callback_data` 与解析 `parse_callback_data`
  集中到 `channels/traits.rs`，QQ/Telegram/frontend 三处调用之。
- skill 布局读写收口：`infrastructure/skills/loader.rs` 暴露
  `skill_dir`/`read_skill_md_in`/`write_skill_md_in`，消除 `systems/experience/` 内
  9 处直接 `std::fs`（残留 `scan_skill_dir` 目录遍历一处，属目录列举，可后续收口）。
- QQ 通道配置写入收口：`qq.rs::persist_allowed_user` 改调
  `ChannelConfigs::append_qq_allowed_user`，文件 IO 知识退出通道模块。
- `[IMAGE:path]` marker 语法说明收口为常量 `ATTACHMENT_MARKER_SYNTAX_HINT`
  （`domain/attachment.rs`）。

### P2：WorkItem 结果处理按知识域重组（2026-08-19）

- `handle_evaluation_work_item_result` 迁至 `systems/evaluation.rs`；
  `handle_summarization_work_item_result` 迁至 `systems/summarization.rs`；
  `handle_profile_generation_invalid` 迁至 `systems/experience/profile_generation.rs`。
- `tool_calling_orchestrator_system` 迁至 `systems/tools/tool_calling.rs`。
- brain 三文件（`brain_decision`/`brain_dispatch`/`brain_llm_builder`）合并到
  `systems/brain/`（经 `git diff --find-copies=30%` 确认为真实 rename，源自
  `transform/` 与 `dispatch/`）；`transform/` 不再引用 `dispatch/` 内部函数。
- `llm_response.rs` 由 1908 行降至 476 行（< 500 验收达成），回归纯路由 + Task 级
  通用 LLM 响应处理。
- 搬迁以 move 为主，函数体无逻辑改动；覆盖测试（evaluation/summarization/profile
  flow）全绿。

### P3：拆解 `handle_tool_action`（2026-08-19）

- `handle_tool_action` 由 ~1206 行拆为 ~200 行纯分派器，按 action kind 拆出 ~15 个
  per-action handler（`dispatch_session_action`/`handle_start_session`/
  `handle_read_session`/`handle_submit_profile_update`/`handle_ask_user` 等），主函数
  只保留分派（验收"任一函数 ≤ 200 行"达成）。
- 微文件合并：`tools/builtin/shell/` 下单工具文件合并为 `shell_tools.rs`。
- 流水线协议（`InFlightToolCall` claim 语义、工具结果落地 + 请求 despawn 协议）集中为
  `orchestrator.rs` 模块级 rustdoc 单一权威。
- 与 spec 处方的偏差：拆解按 action kind 而非执行路径（sync/async/shell），但
  ≤200 行 + 主函数只分派的验收已达成。

### P4：领域类型收紧（2026-08-19）

- ID newtype 化：`TaskId`/`AgentId`/`SessionHandleId` 由 `pub type = Uuid` 改为
  newtype struct，编译器驱动全量替换，杜绝 ID 互传。
- `Task::status`/`last_error` 等状态字段收窄 `pub(crate)`，状态转换统一经 `mark_*`
  方法（恢复结构化 `TaskStatusTransition` 日志）；`WorkItem` 对齐补 `mark_*` 并收窄字段。
- `TaskRoutingPolicy` 字段私有化，合法组合仅经 `conversational`/`event`/
  `scheduled_task` 工厂构造。
- 状态直接赋值断言入 CI：`scripts/check_status_assignment.sh` grep 仓库内对
  `task.status`/`work_item.status` 的直接赋值（豁免 `domain/task.rs`、
  `domain/work_item.rs` 内 `mark_*` 实现），已接入 `.github/workflows/ci.yml`。

### P5：接口与实现对齐（2026-08-19）

- `WorldCommand` 未实现命令变体删除（残留变体均已实现回放，无"跳过"占位）。
- `llm/factory.rs` 四分支同构 match 简化为单一 `GenaiExecutor` 构造（文件降至 17 行）。
- 前置 hook 补 `tool_call_name`/`tool_call_input_json`（`host_api/tool_control.rs`，
  条件拒绝核心用途）。
- `HookPoint` 三重字符串映射（变体/`FromStr`/`as_serialized`/`ALL_NAMES`）用
  `define_hook_points!` 宏单点化（`domain/hook_point.rs`）。
- `HARNESS_PLUGINS_DIR` 默认值去重，统一到 `user_plugins/loader.rs::DEFAULT_PLUGINS_DIR`。
- `llm/registry.rs` `model` 字段改 `Option`，消除 `"placeholder"` 语义不诚实。

### 实施期回归修复（2026-08-20，commit 73ff96a）

- `create_tasks` 子任务索引漏注册：`spawn_create_tasks_messages` 同步
  `index.tasks.insert`，消除 `brain_decision_system` 经索引 O(1) 查找子任务失败、
  静默丢弃 Brain 决策、子任务卡死 `Waiting(Agent)` 的 bug；brain_decision 查不到任务
  时由静默丢弃改为 `warn!` 日志便于定位。补回归测试。
- sync/deny 工具确认路径状态恢复竞态：`tool_dispatch_system` 在
  `ToolRequiresUserConfirmation` 时把 `task.status` 设为 `Waiting(User)`，确认落地后
  未恢复为 `Waiting(ToolExecution)`，导致 `tool_calling_turn_reset_system` 误杀
  `ToolCallingState`、LLM 调用循环永久中断。抽取 `restore_task_to_tool_execution`
  统一 sync/async/executor-missing 三子路径恢复，补 sync + deny 两条回归测试。

### 最终验证

- `cargo test --all-features` 退出码 0：1248 passed / 0 failed / 6 ignored
  （6 个 ignored 均为真实 API 门控测试，非回归掩盖）；`cargo clippy -D warnings`、
  `cargo fmt` 与依赖方向/状态赋值断言脚本由 CI 门禁保证。
