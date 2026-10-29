# 进程所有权变更质量自查

> __状态：自查记录__
> 范围：任务 A，基于 `e0ce22e`；独立质量 reviewer 上次因额度错误未完成，不能视为通过。

## 发现与修正

- __P2：迟到完成覆盖明确停止。__ `refresh_session_state` 和 exec 完成路径在释放 child 锁后
  才写回句柄，可能覆盖并发停止已经记录的 `Stopped`。新增状态保持回归先得到
  `Completed != Stopped` 的失败，再将最终写回统一到 `record_finished` 的 sessions 锁内合并，
  保留明确停止的状态、退出码、超时与结束时间，并返回合并后的句柄。
- __P2：只读快照回写旧状态。__ `read_session` 原来把读取到的旧句柄重新插入注册表，
  可能在并发停止后写回 `Running`。读取现在只生成返回快照，不回写运行状态。

## 验证证据

以下命令使用 `CARGO_TARGET_DIR=/Users/diater/workspace/Harness/target`：

- `cargo test --locked --offline --lib systems::tools::backend::native::tests`：20 项通过。
- `cargo test --locked --offline --test shell_exec_async_test --test shell_tool_flow`：5 项与 26 项通过。
- `cargo clippy --locked --offline --all-targets --all-features -- -D warnings`：通过。
- 修正文档的 markdownlint、修改源文件的 rustfmt 检查和 `git diff --check`：通过。
- 全仓 fmt 检查遇到并行任务 C 尚在编辑的测试格式差异，本提交不修改该任务文件。

## 验证边界

新增回归直接重放迟到完成写回，确定性检验终态合并；没有声称穷举线程调度顺序。
本记录属于实现者自查，不替代后续独立代码质量审查。
实例死亡的独立守护保障属于任务 B，本次未实现。
