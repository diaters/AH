# ChannelFrontend 滚动撤回竞态修复 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 ChannelFrontend 中 on_sent 异步回调与同步 Text(LLMReply) 之间的竞态——LLMReply 到达时 last_status_msg 为空导致状态消息未被撤回。

**Architecture:** 新增 `pending_reply_recall: HashSet<(task_id, recipient)>` 标记。`Text(LLMReply)` 处理器在 `last_status_msg[key]` 为空时设置标记；`on_sent` 回调拿到新 msg_id 后检查标记 → 入队 Recall 撤回刚发出的状态消息 + 清理 `last_status_msg`。`TaskStatusChanged` 处理器不触碰此标记。

**Tech Stack:** Rust, tokio::sync::mpsc, std::sync::RwLock

## Global Constraints

- 所有改动仅限于 `src/channels/frontend.rs`，不修改 trait 定义、不修改其他模块
- 遵循现有错误处理模式：`try_read`/`try_write` 静默跳过锁争用
- `on_sent` 闭包中通过 `outbound_tx.send()` 入队 Recall，而非调用 `self.enqueue_recall()`（因闭包无法借用 self）
- `TaskStatusChanged` 处理器**不设置** `pending_reply_recall` 标记——标记仅在 `Text(LLMReply)` 的竞态分支中设置
- `on_sent` 中 Recall 后必须从 `last_status_msg` 中移除 key，避免重复 Recall

---

### Task 1: 新增 `pending_reply_recall` 字段 + 初始化 + TaskCleared 清理

**Files:**
- Modify: `src/channels/frontend.rs:18-27`, `:34-42`, `:324-332`

**Interfaces:**
- Consumes: 无（首个 Task）
- Produces: `ChannelFrontend.pending_reply_recall` 字段，类型 `Arc<RwLock<HashSet<(String, String)>>>`，供 Task 2、Task 3 的闭包捕获

- [ ] **Step 1: 在 struct 中新增字段**

在 `src/channels/frontend.rs` 第 26 行 `task_finalized` 之后插入：

```rust
    /// (task_id, recipient) — 标记 LLMReply 已到达但 on_sent 尚未回写新 msg_id。
    /// on_sent 回调在新 msg_id 确认后，若此集合中包含该 key，
    /// 则立即发起 Recall 撤回刚发送的状态消息，并清理 last_status_msg。
    pending_reply_recall: Arc<RwLock<HashSet<(String, String)>>>,
```

- [ ] **Step 2: 在 `new()` 中初始化**

在第 40 行 `task_finalized` 之后插入：

```rust
            pending_reply_recall: Arc::new(RwLock::new(HashSet::new())),
```

- [ ] **Step 3: 在 `TaskCleared` 处理器中添加清理**

在第 330 行 `task_finalized.remove` 之后插入：

```rust
                if let Ok(mut set) = self.pending_reply_recall.try_write() {
                    set.retain(|(tid, _)| tid != &task_id_str);
                }
```

- [ ] **Step 4: 编译检查**

Run: `cargo check -p harness`
Expected: 编译通过，无警告（`pending_reply_recall` 暂时未被读取，但 `dead_code` 在 pub struct 字段上不报警）

- [ ] **Step 5: 运行现有测试验证回归**

Run: `cargo test -p harness -- channels::frontend::tests`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src/channels/frontend.rs
git commit -m "feat(channels): add pending_reply_recall field to ChannelFrontend

Add HashSet field for tracking (task, recipient) pairs where LLMReply
arrived before on_sent callback wrote back the new msg_id. This field
will be used in subsequent commits to implement the deferred recall.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: 修改 `Text(LLMReply)` 处理器——竞态分支设置标记

**Files:**
- Modify: `src/channels/frontend.rs:159-181`

**Interfaces:**
- Consumes: `ChannelFrontend.pending_reply_recall`（Task 1 产出）
- Produces: `pending_reply_recall` 中的 (task_id, recipient) 条目，供 Task 3 的 `on_sent` 检查

- [ ] **Step 1: 修改 Text(LLMReply) 中的 Recall 逻辑**

将现有的 LLMReply Recall 代码（第 159-181 行）：

```rust
                    // LLM 回复到达时，撤回该 task+recipient 的最终态状态消息
                    if message_kind == MessageKind::LLMReply
                        && let Some(tid) = task_id
                    {
                        let key = (tid.to_string(), channel_id.user_id.clone());
                        if let Ok(map) = self.last_status_msg.try_read()
                            && let Some(msg_id) = map.get(&key).cloned()
                        {
                            drop(map);
                            self.enqueue_recall(
                                channel_id.user_id.clone(),
                                channel_id.thread_id.clone(),
                                msg_id,
                            );
                            if let Ok(mut map) = self.last_status_msg.try_write() {
                                map.remove(&key);
                            }
                        }
                        if let Ok(mut set) = self.task_finalized.try_write() {
                            set.insert(tid.to_string());
                        }
                    }
```

替换为：

```rust
                    // LLM 回复到达时，撤回该 task+recipient 的最终态状态消息
                    if message_kind == MessageKind::LLMReply
                        && let Some(tid) = task_id
                    {
                        let key = (tid.to_string(), channel_id.user_id.clone());
                        if let Ok(map) = self.last_status_msg.try_read()
                            && let Some(msg_id) = map.get(&key).cloned()
                        {
                            // 正常路径：last_status_msg 有值 → 直接 Recall
                            drop(map);
                            self.enqueue_recall(
                                channel_id.user_id.clone(),
                                channel_id.thread_id.clone(),
                                msg_id,
                            );
                            if let Ok(mut map) = self.last_status_msg.try_write() {
                                map.remove(&key);
                            }
                        } else {
                            // 竞态路径：last_status_msg 为空（on_sent 尚未回写）
                            // → 设置标记，委托 on_sent 兜底 Recall
                            if let Ok(mut pending) = self.pending_reply_recall.try_write() {
                                pending.insert(key);
                            }
                        }
                        if let Ok(mut set) = self.task_finalized.try_write() {
                            set.insert(tid.to_string());
                        }
                    }
```

- [ ] **Step 2: 编译检查**

Run: `cargo check -p harness`
Expected: 编译通过，无警告

- [ ] **Step 3: 运行现有测试验证回归**

Run: `cargo test -p harness -- channels::frontend::tests`
Expected: 全部 PASS（`llm_reply_recalls_last_status` 测试中 on_sent 先执行，走正常路径，不受影响）

- [ ] **Step 4: Commit**

```bash
git add src/channels/frontend.rs
git commit -m "feat(channels): set pending_reply_recall in LLMReply race path

When Text(LLMReply) arrives and last_status_msg is empty (on_sent has
not yet written back the new msg_id), insert a (task, recipient) marker
into pending_reply_recall so on_sent can defer the recall.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: 修改 `on_sent` 回调——检查标记 + Recall + 清理 last_status_msg

**Files:**
- Modify: `src/channels/frontend.rs:300-310`

**Interfaces:**
- Consumes: `pending_reply_recall` 条目（Task 2 产出），`outbound_tx`、`channel_name`（`ChannelFrontend` 现有字段）
- Produces: 出向队列中的 Recall 条目

- [ ] **Step 1: 重构 `on_sent` 闭包捕获与逻辑**

将现有的 `on_sent` 构造代码（第 300-310 行）：

```rust
                    // 准备 on_sent 回调：更新 last_status_msg
                    let last_status_msg = self.last_status_msg.clone();
                    let on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>> =
                        Some(Box::new(move |msg_id: Option<String>| {
                            if let Some(id) = msg_id
                                && let Ok(mut map) = last_status_msg.try_write()
                            {
                                map.insert(key, id);
                            }
                        }));
```

替换为：

```rust
                    // 准备 on_sent 回调：更新 last_status_msg + 检查 pending_reply_recall
                    let last_status_msg = self.last_status_msg.clone();
                    let pending_reply_recall = self.pending_reply_recall.clone();
                    let outbound_tx = self.outbound_tx.clone();
                    let channel_name = self.channel_name.clone();
                    let recipient = channel_id.user_id.clone();
                    let thread_id = channel_id.thread_id.clone();
                    let on_sent: Option<Box<dyn FnOnce(Option<String>) + Send + Sync>> =
                        Some(Box::new(move |msg_id: Option<String>| {
                            // 1. 保存新 msg_id（现行为）
                            if let Some(ref id) = msg_id
                                && let Ok(mut map) = last_status_msg.try_write()
                            {
                                map.insert(key.clone(), id.clone());
                            }
                            // 2. ⬇ 新增：检查 pending_reply_recall → 撤回刚发送的状态消息
                            if let Ok(mut pending) = pending_reply_recall.try_write() {
                                if pending.remove(&key) {
                                    if let Some(id) = msg_id {
                                        // 清理 last_status_msg，避免后续 LLMReply 重复 Recall
                                        if let Ok(mut map) = last_status_msg.try_write() {
                                            map.remove(&key);
                                        }
                                        let recall_entry = OutboundEntry {
                                            channel_name,
                                            message: ChannelOutboundMessage {
                                                recipient,
                                                thread_id,
                                                content: id,
                                                parse_mode: None,
                                                reply_markup: None,
                                                attachments: vec![],
                                                message_kind: MessageKind::Recall,
                                            },
                                            on_sent: None,
                                        };
                                        let _ = outbound_tx.send(recall_entry);
                                    }
                                }
                            }
                        }));
```

注意：`key` 在 `for channel_id in recipients` 循环内定义（第 283 行），闭包通过 `move` 捕获它——由于闭包只运行一次（`FnOnce`），这符合 `key` 的所有权语义。所有新增捕获变量均为 `clone`（`Arc` 或 `String`），无所有权冲突。

- [ ] **Step 2: 编译检查**

Run: `cargo check -p harness`
Expected: 编译通过，无警告

- [ ] **Step 3: 运行现有测试验证回归**

Run: `cargo test -p harness -- channels::frontend::tests`
Expected: 全部 PASS

- [ ] **Step 4: Commit**

```bash
git add src/channels/frontend.rs
git commit -m "feat(channels): on_sent checks pending_reply_recall for deferred recall

When on_sent fires and finds a (task, recipient) marker in
pending_reply_recall, it enqueues a Recall for the freshly sent status
message and cleans up last_status_msg to prevent duplicate recalls.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: 测试覆盖

**Files:**
- Modify: `src/channels/frontend.rs`（`#[cfg(test)]` 模块末尾，第 934 行前）

- [ ] **Step 1: 添加 `normal_status_transition_does_not_recall_new_msg` 测试（关键反例）**

验证：正常状态变更（无 LLMReply）→ 每次 on_sent 后不产生 Recall，状态消息保留。

```rust
    #[test]
    fn normal_status_transition_does_not_recall_new_msg() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let running_entry = rx.try_recv().expect("running status");
        assert_eq!(running_entry.message.message_kind, MessageKind::TaskStatus);
        (running_entry.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态 — 应 Recall msg_1，但不应对 msg_2 设 pending 标记
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let recall_entry = rx.try_recv().expect("recall msg_1");
        assert_eq!(recall_entry.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry.message.content, "msg_1");
        let waiting_entry = rx.try_recv().expect("waiting status");
        assert_eq!(waiting_entry.message.message_kind, MessageKind::TaskStatus);
        // 触发 on_sent — 不应产生 Recall（无 LLMReply 到达 → 无 pending 标记）
        (waiting_entry.on_sent.unwrap())(Some("msg_2".to_string()));
        assert!(rx.try_recv().is_err(), "no recall after normal on_sent");

        // Done 状态 — 同理
        fe.push_event(EngineEvent::TaskStatusChanged {
            target,
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Waiting),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let recall_entry2 = rx.try_recv().expect("recall msg_2");
        assert_eq!(recall_entry2.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry2.message.content, "msg_2");
        let done_entry = rx.try_recv().expect("done status");
        assert_eq!(done_entry.message.message_kind, MessageKind::TaskStatus);
        (done_entry.on_sent.unwrap())(Some("msg_3".to_string()));
        assert!(rx.try_recv().is_err(), "no recall after normal on_sent");
    }
```

- [ ] **Step 2: 添加 `llm_reply_recalls_pending_status` 测试（竞态场景）**

验证：LLMReply 在 on_sent 之前到达 → on_sent 兜底 Recall。

```rust
    #[test]
    fn llm_reply_recalls_pending_status() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry1 = rx.try_recv().expect("running status");
        (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态 — Recall msg_1，发送 msg_2（on_sent 尚未执行）
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let _recall = rx.try_recv().expect("recall msg_1");
        let status_entry = rx.try_recv().expect("waiting status");
        // ⚠️ 故意不执行 on_sent

        // LLMReply 到达 — last_status_msg 为空 → 应设置 pending 标记
        fe.push_event(EngineEvent::Text {
            target: target.clone(),
            role: MessageRole::Agent,
            content: "done".to_string(),
            task_id: Some(task_id),
        });
        let llm_entry = rx.try_recv().expect("llm reply");
        assert_eq!(llm_entry.message.message_kind, MessageKind::LLMReply);
        // 无即时 Recall（last_status_msg 为空）

        // on_sent 稍后执行 → 应入队 Recall(msg_2)
        (status_entry.on_sent.unwrap())(Some("msg_2".to_string()));
        let deferred_recall = rx.try_recv().expect("deferred recall from on_sent");
        assert_eq!(deferred_recall.message.message_kind, MessageKind::Recall);
        assert_eq!(deferred_recall.message.content, "msg_2");
        assert!(rx.try_recv().is_err(), "no more messages");
    }
```

- [ ] **Step 3: 添加 `pending_recall_cleaned_on_task_cleared` 测试**

验证：TaskCleared 后 pending_reply_recall 无残留，on_sent 不 Recall。

```rust
    #[test]
    fn pending_recall_cleaned_on_task_cleared() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry1 = rx.try_recv().expect("running status");
        (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态（on_sent 未执行）
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let _recall = rx.try_recv().expect("recall msg_1");
        let status_entry = rx.try_recv().expect("waiting status");

        // LLMReply 到达 → 设置 pending 标记
        fe.push_event(EngineEvent::Text {
            target: target.clone(),
            role: MessageRole::Agent,
            content: "done".to_string(),
            task_id: Some(task_id),
        });
        let _llm = rx.try_recv().expect("llm reply");

        // TaskCleared — 应清理 pending_reply_recall
        fe.push_event(EngineEvent::TaskCleared {
            target,
            task_id,
        });
        assert!(rx.try_recv().is_err(), "TaskCleared produces no outbound");

        // on_sent 执行 — 不应触发 Recall（标记已被清理）
        (status_entry.on_sent.unwrap())(Some("msg_2".to_string()));
        assert!(rx.try_recv().is_err(), "recall should not fire after clear");
    }
```

- [ ] **Step 4: 添加 `pending_recall_not_set_if_last_status_exists` 测试**

验证：LLMReply 到达时 last_status_msg 有值 → 走正常路径，不设标记。

```rust
    #[test]
    fn pending_recall_not_set_if_last_status_exists() {
        use uuid::Uuid;
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        let task_id: TaskId = Uuid::nil();
        let cid = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let target = EventTarget::Directed(vec![cid]);

        // Running 状态 → on_sent 已执行
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: Some(TaskStatusKind::Pending),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let entry1 = rx.try_recv().expect("running status");
        (entry1.on_sent.unwrap())(Some("msg_1".to_string()));

        // Waiting 状态 → on_sent 已执行
        fe.push_event(EngineEvent::TaskStatusChanged {
            target: target.clone(),
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Waiting,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        let _recall = rx.try_recv().expect("recall msg_1");
        let status_entry = rx.try_recv().expect("waiting status");
        (status_entry.on_sent.unwrap())(Some("msg_2".to_string()));

        // LLMReply 到达 — last_status_msg 有值 → 正常 Recall，不设 pending 标记
        fe.push_event(EngineEvent::Text {
            target,
            role: MessageRole::Agent,
            content: "done".to_string(),
            task_id: Some(task_id),
        });
        let recall_entry = rx.try_recv().expect("recall msg_2");
        assert_eq!(recall_entry.message.message_kind, MessageKind::Recall);
        assert_eq!(recall_entry.message.content, "msg_2");
        let llm_entry = rx.try_recv().expect("llm reply");
        assert_eq!(llm_entry.message.message_kind, MessageKind::LLMReply);
        assert!(rx.try_recv().is_err(), "no more messages");
    }
```

- [ ] **Step 5: 运行所有新增和现有测试**

Run: `cargo test -p harness -- channels::frontend::tests`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src/channels/frontend.rs
git commit -m "test(channels): add tests for pending_reply_recall race fix

Add four test cases:
- normal_status_transition_does_not_recall_new_msg: key regression test
  ensuring status messages are preserved without LLMReply
- llm_reply_recalls_pending_status: deferred recall via on_sent
- pending_recall_cleaned_on_task_cleared: cleanup on task clear
- pending_recall_not_set_if_last_status_exists: normal path unaffected

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: 验证与提交

- [ ] **Step 1: 完整编译检查**

Run: `cargo check --all-features`
Expected: 编译通过，无警告

- [ ] **Step 2: Clippy 检查**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无新警告

- [ ] **Step 3: 完整测试套件**

Run: `cargo test --all-features`
Expected: 全部 PASS

- [ ] **Step 4: 格式化检查**

Run: `cargo fmt --all --check`
Expected: 无格式问题

- [ ] **Step 5: Squash 合并（可选）**

如需将 Task 1-4 的 4 个 commit 合并为一个：

```bash
git rebase -i HEAD~4
# 将后 3 个 commit 标记为 squash
```

或保持独立 commit 不做 squash。
