# ChannelFrontend 滚动撤回竞态修复设计规约

> 当前有效 — 修复 on_sent 异步回调与同步 Text 事件之间的竞态

## 问题

### 现象

QQ 通道中，Agent 在产生 LLM 回复后，最后一条状态消息 `[935fe98e]: 运行中 → 等待中 @bug-workflow-specialist` 未被撤回，和 Agent 回复文本同时显示给用户。

### 根因

`ChannelFrontend.push_event(TaskStatusChanged)` 处理状态变更时：

1. 从 `last_status_msg` 中找到旧 msg_id → 发起 Recall（异步入队）
2. **从 `last_status_msg` 移除旧 msg_id**
3. 发送新状态消息，注册 `on_sent` 回调 → **异步等待网络发完才回写新 msg_id**

若 `push_event(Text(LLMReply))` 在上述两步之间执行（同 ECS 帧，间隔 <3ms），`last_status_msg` 为空 → 不发起 Recall → 状态消息残留。

```
ECS 帧:
  push_event(TaskStatusChanged)
    → Recall 旧消息 / 移除旧 msg_id / on_sent 异步
    → last_status_msg = ∅
  push_event(Text(LLMReply))
    → last_status_msg 为空 → ❌ 无 Recall
    → LLMReply 直接发送
    → 用户在 QQ 看到「状态」+「回复」并存
```

## 修复方案

### 核心思路

在 `Text(LLMReply)` 处理器中检测竞态条件——当 `last_status_msg[key]` 为空时（说明 on_sent 尚未回写新 msg_id），设置 `pending_reply_recall` 标记。`on_sent` 回调拿到新 msg_id 后检查此标记 → 立即发起 Recall（撤回刚发送的状态消息）。

**关键设计决策：标记的设置时机在 `Text(LLMReply)` 而非 `TaskStatusChanged`。** 这样：

- 正常状态变更（无 LLMReply）：不设标记 → on_sent 不 Recall → 状态消息保留 ✓
- 竞态场景（LLMReply 在 on_sent 之前到达）：LLMReply 处理器设标记 → on_sent 兜底 Recall ✓
- 正常 LLMReply（on_sent 已执行）：`last_status_msg` 有值 → LLMReply 处理器直接 Recall ✓

### 出向处理器时序

`ChannelManager` 的出向处理器（`src/channels/manager.rs:130-154`）是单线程 FIFO 循环：`channel.send()` 完成后**同步调用** `on_sent`，然后才处理队列中的下一条。

因此，`push_event` 入队的顺序即为处理顺序：

```
[Recall(旧 msg_id)] → [TaskStatus(新 msg_id)] → [on_sent 同步执行] → [LLMReply 文本]
```

**竞态场景的实际时序：**

```
ECS 帧内 push_event 入队顺序：
  1. Recall(旧 msg_id)        ← TaskStatusChanged 处理
  2. TaskStatus(新 msg_id)    ← TaskStatusChanged 处理
  3. LLMReply 文本             ← Text 处理

出向处理器 FIFO 处理：
  处理 1: Recall(旧 msg_id) → 发送到 QQ
  处理 2: TaskStatus(新 msg_id) → 发送到 QQ → on_sent 同步执行
    → on_sent: insert(msg_2) 到 last_status_msg
    → on_sent: 检查 pending_reply_recall → 有标记 → Recall(msg_2) 入队到 FIFO 尾部
  处理 3: LLMReply 文本 → 发送到 QQ
  处理 4: Recall(msg_2) → 发送到 QQ → 状态消息被撤回
```

用户看到的顺序：旧状态消失 → 新状态出现 → LLMReply 出现 → 新状态消失。

**非竞态场景（on_sent 在 LLMReply 入队前已执行）：** `last_status_msg` 有值 → LLMReply 处理器直接 Recall → 无需经过 `pending_reply_recall`。

### 数据结构

在 `ChannelFrontend` 中新增一个 `HashSet`：

```rust
pub struct ChannelFrontend {
    // ... 现有字段 ...
    /// (task_id, recipient) — 标记 LLMReply 已到达但 on_sent 尚未回写新 msg_id。
    /// on_sent 回调在新 msg_id 确认后，若此集合中包含该 key，
    /// 则立即发起 Recall 撤回刚发送的状态消息，并清理 last_status_msg。
    pending_reply_recall: Arc<RwLock<HashSet<(String, String)>>>,
}
```

### 四点修改

#### 1. `push_event(Text(LLMReply))` 处理器

**此为修改核心。** 现有逻辑在 `last_status_msg[key]` 有值时 Recall，为空时跳过。改为：为空时设置 `pending_reply_recall` 标记。

```rust
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
        // 竞态路径：last_status_msg 为空 → 设置标记，委托 on_sent 兜底
        if let Ok(mut pending) = self.pending_reply_recall.try_write() {
            pending.insert(key);
        }
    }
    if let Ok(mut set) = self.task_finalized.try_write() {
        set.insert(tid.to_string());
    }
}
```

#### 2. `on_sent` 回调

捕获 `pending_reply_recall`、`outbound_tx` 等，在保存新 msg_id 后检查标记。若标记存在 → Recall 刚发送的消息 + 清理 `last_status_msg`：

```rust
let on_sent = Some(Box::new(move |msg_id: Option<String>| {
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
                    channel_name: channel_name.clone(),
                    message: ChannelOutboundMessage {
                        recipient: recipient.clone(),
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

关键：`on_sent` 中 Recall 后**必须**从 `last_status_msg` 中移除 key，否则后续到达的 LLMReply 会从 `last_status_msg` 取到同一 msg_id 并再次 Recall。

#### 3. `TaskCleared` 处理器——配套清理

```rust
EngineEvent::TaskCleared { task_id, .. } => {
    let task_id_str = task_id.to_string();
    if let Ok(mut map) = self.last_status_msg.try_write() {
        map.retain(|(tid, _), _| tid != &task_id_str);
    }
    if let Ok(mut set) = self.task_finalized.try_write() {
        set.remove(&task_id_str);
    }
    // ⬇ 新增：清理 pending_reply_recall
    if let Ok(mut set) = self.pending_reply_recall.try_write() {
        set.retain(|(tid, _)| tid != &task_id_str);
    }
}
```

#### 4. 初始化

```rust
Self {
    kind,
    channel_name: channel_name.into(),
    outbound_tx,
    last_status_msg: Arc::new(RwLock::new(HashMap::new())),
    task_finalized: Arc::new(RwLock::new(HashSet::new())),
    pending_reply_recall: Arc::new(RwLock::new(HashSet::new())),
}
```

### 不修改处

- `push_event(TaskStatusChanged)` —— 保持现有行为，不触碰 `pending_reply_recall`
- `Frontend` trait —— 接口不变
- 其他模块 —— 无影响

## 影响范围

| 维度 | 评估 |
|---|---|
| 改动范围 | 仅 `src/channels/frontend.rs`，< 40 行净增 |
| 接口变动 | 无。`Frontend` trait 不变，`ChannelFrontend` 对外可见性不变 |
| 风险等级 | 低。复用现有 `enqueue_recall` / `try_read` / `try_write` 模式 |
| 回滚代价 | 去掉新增字段和 3 处代码即可 |
| 测试覆盖 | 需覆盖：正常状态变更不 Recall（关键反例）、竞态 LLMReply 触发 pending recall、TaskCleared 清理 |

## 测试策略

### 新增测试用例（在 `src/channels/frontend.rs` 的 `#[cfg(test)]` 中）

| 用例 | 验证点 |
|---|---|
| `normal_status_transition_does_not_recall_new_msg` | Running→Waiting→Done，每步触发 on_sent，**不产生任何 Recall** |
| `llm_reply_recalls_pending_status` | 竞态：LLMReply 在 on_sent 之前到达 → on_sent 兜底 Recall |
| `pending_recall_cleaned_on_task_cleared` | TaskCleared 后 pending_reply_recall 无残留，on_sent 不 Recall |
| `pending_recall_not_set_if_last_status_exists` | LLMReply 到达时 last_status_msg 有值 → 走正常路径，不设标记 |

### 现有测试不受影响

- `task_status_rolling_recall` — 状态链正常 Recall（`TaskStatusChanged` 不触碰 `pending_reply_recall`）
- `llm_reply_recalls_last_status` — LLMReply 在 on_sent 已执行后 Recall（走正常路径）
- `ignores_status_change_broadcast` — Broadcast 不处理
- `task_failed_preserves_final_status` — Failed 不 Recall

## 残留场景说明

如果 on_sent 永远不执行（网络错误、通道断开），`pending_reply_recall` 标记会残留在集合中。不影响功能（标记只在 on_sent 中被消费），但会泄漏少量内存。`TaskCleared` 会兜底清理同一 task_id 的所有标记。此为可接受的 trade-off。

## 相关问题

无。此修复不引入新的跨模块依赖或架构变更。
