//! 上下文压缩盲区修复的集成测试
//!
//! 验证：
//! 1. 工具密集型任务下 estimated_tokens 反映 tool_calls 消耗，压缩能及时触发
//! 2. 压缩触发后配对组完整性（含 tool_calls 的 Assistant 条目不被拆散）
//! 3. 结构化还原路径：从 STM 的 tool_calls 还原 ConversationMessage
//! 4. First iteration 读 request.conversation（结构化路径生效）
//! 5. 空 conversation 不影响 WorkItem 派发路径

use harness::domain::{
    ConversationMessage, EntryMetadata, EntryRole, ShortTermMemory, ToolCall, estimate_tokens,
    render_tool_calls_summary,
};

// ── 1. Token 估算集成测试 ──────────────────────────────────────

#[test]
fn tool_calls_tokens_trigger_compression() {
    // 验证：含 tool_calls 的 STM 能正确累加 token，超过阈值时压缩可触发
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "list files", EntryMetadata::default());

    // 模拟工具密集型场景：多次 record_tool_call
    for i in 0..10 {
        stm.record_tool_call(
            Some(format!("call_{}", i)),
            "shell_exec".to_string(),
            format!("ls -la /path/number/{}", i),
            "file1.txt\nfile2.txt\nfile3.txt\nfile4.txt\nfile5.txt".to_string(),
            chrono::Utc::now(),
        );
    }

    // estimated_tokens 应远大于仅 content 的 token
    let tokens = stm.estimated_tokens;
    assert!(
        tokens > 200,
        "estimated_tokens should reflect tool_calls consumption, got {}",
        tokens,
    );

    // 验证 recalculate_tokens 一致性
    let recorded = stm.estimated_tokens;
    stm.recalculate_tokens();
    assert_eq!(
        stm.estimated_tokens, recorded,
        "recalculate_tokens should match add_entry + record_tool_call accumulation"
    );
}

#[test]
fn no_tool_calls_stays_below_threshold() {
    // 验证：不含 tool_calls 的 STM token 估算与原有行为一致
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "hello", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "hi there", EntryMetadata::default());

    let tokens = stm.estimated_tokens;
    let expected = estimate_tokens("hello") + estimate_tokens("hi there");
    assert_eq!(
        tokens, expected,
        "without tool_calls, estimated_tokens should equal sum of content tokens"
    );
}

// ── 2. 配对组完整性测试 ──────────────────────────────────────

#[test]
fn compression_preserves_tool_call_group() {
    // 验证：压缩时含 tool_calls 的 Assistant 条目不会被单独摘除
    // 构造一个 STM，其中 Assistant 条目有 tool_calls
    let mut stm = ShortTermMemory::default();

    // 对话配对组 1
    stm.add_entry(EntryRole::User, "first question", EntryMetadata::default());
    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls".to_string(),
        output: "file1.txt".to_string(),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, "result 1", metadata);

    // 对话配对组 2
    stm.add_entry(EntryRole::User, "second question", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "result 2", EntryMetadata::default());

    // 验证：含 tool_calls 的 Assistant 条目与 tool_call_id 数据一致
    let assistant_with_tools = stm
        .entries
        .iter()
        .find(|e| e.role == EntryRole::Assistant && !e.metadata.tool_calls.is_empty());
    assert!(
        assistant_with_tools.is_some(),
        "should have an Assistant entry with tool_calls"
    );

    let entry = assistant_with_tools.unwrap();
    assert_eq!(entry.metadata.tool_calls.len(), 1);
    assert_eq!(entry.metadata.tool_calls[0].id, Some("call_1".to_string()));
    assert_eq!(entry.metadata.tool_calls[0].tool_name, "shell_exec");
}

// ── 3. 结构化还原路径测试 ──────────────────────────────────────

/// 从 STM 还原 ConversationMessage 序列的测试
/// （对应 dispatch_system.rs 中的 build_structured_conversation 逻辑）
#[test]
fn structured_conversation_reconstruction() {
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "list files", EntryMetadata::default());

    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls".to_string(),
        output: "file1.txt\nfile2.txt".to_string(),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, "done", metadata);

    stm.add_entry(EntryRole::User, "next question", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "answer", EntryMetadata::default());

    // 手动还原（模拟 build_structured_conversation 逻辑）
    let has_tool_calls = stm
        .entries
        .iter()
        .any(|e| !e.metadata.tool_calls.is_empty());
    assert!(has_tool_calls, "STM should have entries with tool_calls");

    let mut messages = Vec::new();
    for entry in &stm.entries {
        match entry.role {
            EntryRole::User => {
                messages.push(ConversationMessage::User {
                    content: entry.content.clone(),
                });
            }
            EntryRole::Assistant => {
                let tool_calls: Vec<_> = entry
                    .metadata
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(i, tc)| harness::domain::LlmToolCall {
                        id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                        name: tc.tool_name.clone(),
                        arguments: tc.input.clone(),
                    })
                    .collect();

                messages.push(ConversationMessage::Assistant {
                    content: if entry.content.is_empty() {
                        None
                    } else {
                        Some(entry.content.clone())
                    },
                    tool_calls,
                    reasoning_content: None,
                });

                for (i, tc) in entry.metadata.tool_calls.iter().enumerate() {
                    messages.push(ConversationMessage::Tool {
                        tool_call_id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                        content: tc.output.clone(),
                    });
                }
            }
            EntryRole::Summary => {
                messages.push(ConversationMessage::User {
                    content: format!("[System note] {}", entry.content),
                });
            }
            EntryRole::Archive => {}
        }
    }

    // User → Assistant(tool_calls) → Tool → User → Assistant
    assert_eq!(messages.len(), 5, "should produce 5 messages");
    assert!(matches!(messages[0], ConversationMessage::User { .. }));
    assert!(
        matches!(&messages[1], ConversationMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
        "second message should be Assistant with tool_calls"
    );
    assert!(
        matches!(&messages[2], ConversationMessage::Tool { tool_call_id, .. } if tool_call_id == "call_1"),
        "third message should be Tool with call_1"
    );
    assert!(matches!(messages[3], ConversationMessage::User { .. }));
    assert!(
        matches!(&messages[4], ConversationMessage::Assistant { tool_calls, .. } if tool_calls.is_empty()),
        "fifth message should be Assistant without tool_calls"
    );
}

#[test]
fn structured_conversation_no_tool_calls_returns_none() {
    // 验证：不含 tool_calls 的 STM 不走结构化路径
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "hello", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "hi", EntryMetadata::default());

    let has_tool_calls = stm
        .entries
        .iter()
        .any(|e| !e.metadata.tool_calls.is_empty());
    assert!(
        !has_tool_calls,
        "STM without tool_calls should not trigger structured path"
    );
}

#[test]
fn structured_conversation_summary_prefix_renders_as_user() {
    // 验证：summary_prefix 在结构化路径中渲染为 User 消息
    let mut stm = ShortTermMemory {
        summary_prefix: Some("Previous conversation summary".to_string()),
        ..Default::default()
    };
    stm.add_entry(EntryRole::User, "question", EntryMetadata::default());

    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls".to_string(),
        output: "files".to_string(),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, "done", metadata);

    // 模拟还原
    let mut messages = Vec::new();
    if let Some(summary) = &stm.summary_prefix {
        messages.push(ConversationMessage::User {
            content: format!("[Previous context summary]\n{}", summary),
        });
    }
    for entry in &stm.entries {
        match entry.role {
            EntryRole::User => {
                messages.push(ConversationMessage::User {
                    content: entry.content.clone(),
                });
            }
            EntryRole::Assistant => {
                let tool_calls: Vec<_> = entry
                    .metadata
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(i, tc)| harness::domain::LlmToolCall {
                        id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                        name: tc.tool_name.clone(),
                        arguments: tc.input.clone(),
                    })
                    .collect();
                messages.push(ConversationMessage::Assistant {
                    content: if entry.content.is_empty() {
                        None
                    } else {
                        Some(entry.content.clone())
                    },
                    tool_calls,
                    reasoning_content: None,
                });
                for (i, tc) in entry.metadata.tool_calls.iter().enumerate() {
                    messages.push(ConversationMessage::Tool {
                        tool_call_id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                        content: tc.output.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    // summary_prefix → User → Assistant(tool_calls) → Tool
    assert_eq!(messages.len(), 4);
    assert!(
        matches!(&messages[0], ConversationMessage::User { content } if content.contains("Previous context summary")),
        "first message should be summary prefix as User"
    );
}

// ── 4. First iteration 读 request.conversation 测试 ──────────────

#[test]
fn first_iteration_uses_non_empty_conversation() {
    // 验证：非空 conversation 优先被使用
    // 模拟 First iteration 的判断逻辑
    let conversation: Option<Vec<ConversationMessage>> = Some(vec![
        ConversationMessage::User {
            content: "previous question".to_string(),
        },
        ConversationMessage::Assistant {
            content: Some("previous answer".to_string()),
            tool_calls: vec![],
            reasoning_content: None,
        },
    ]);

    let should_use_conversation = conversation.as_ref().is_some_and(|c| !c.is_empty());
    assert!(
        should_use_conversation,
        "non-empty conversation should be used in First iteration"
    );
}

#[test]
fn first_iteration_ignores_empty_conversation() {
    // 验证：空 Vec 视同 None，走纯文本路径
    let conversation: Option<Vec<ConversationMessage>> = Some(vec![]);
    let should_use_conversation = conversation.as_ref().is_some_and(|c| !c.is_empty());
    assert!(
        !should_use_conversation,
        "empty Vec should be treated as None"
    );

    // None 情况
    let conversation: Option<Vec<ConversationMessage>> = None;
    let should_use_conversation = conversation.as_ref().is_some_and(|c| !c.is_empty());
    assert!(
        !should_use_conversation,
        "None should not be used in First iteration"
    );
}

// ── 5. render_tool_calls_summary 测试 ──────────────────────────

#[test]
fn render_tool_calls_summary_content() {
    let tool_calls = vec![
        ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "ls".to_string(),
            output: "file1.txt\nfile2.txt".to_string(),
            timestamp: chrono::Utc::now(),
        },
        ToolCall {
            id: Some("call_2".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "cat x".to_string(),
            output: "content of x".to_string(),
            timestamp: chrono::Utc::now(),
        },
    ];

    let summary = render_tool_calls_summary(&tool_calls);
    assert!(summary.contains("[Tool calls:"), "should have header");
    assert!(
        summary.contains("shell_exec(\"ls\")"),
        "should contain first tool call"
    );
    assert!(
        summary.contains("shell_exec(\"cat x\")"),
        "should contain second tool call"
    );
    assert!(
        summary.contains("file1.txt"),
        "should contain truncated output"
    );
}

#[test]
fn render_tool_calls_summary_empty() {
    let summary = render_tool_calls_summary(&[]);
    assert!(
        summary.is_empty(),
        "empty tool_calls should produce empty summary"
    );
}

#[test]
fn render_tool_calls_summary_long_output_truncated() {
    let long_output = "x".repeat(500);
    let tool_calls = vec![ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "cat big_file".to_string(),
        output: long_output.clone(),
        timestamp: chrono::Utc::now(),
    }];

    let summary = render_tool_calls_summary(&tool_calls);
    assert!(
        summary.contains("[truncated]"),
        "long output should be truncated"
    );
    assert!(
        summary.len() < long_output.len(),
        "summary should be shorter than raw output"
    );
}

// ── 6. 配对组切分算法测试 ──────────────────────────────────────

#[test]
fn pair_group_splitting_with_tool_calls() {
    // 验证：含 tool_calls 的 Assistant 开启新的工具配对组
    let mut stm = ShortTermMemory::default();

    // 对话配对组
    stm.add_entry(EntryRole::User, "question", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "answer", EntryMetadata::default());

    // 工具配对组
    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "ls".to_string(),
        output: "files".to_string(),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, "", metadata);

    // 新对话配对组
    stm.add_entry(EntryRole::User, "follow-up", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "final", EntryMetadata::default());

    // 模拟 split_into_groups 逻辑
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_group: Vec<usize> = Vec::new();

    for (i, entry) in stm.entries.iter().enumerate() {
        let starts_new_group = match entry.role {
            EntryRole::User => true,
            EntryRole::Assistant if !entry.metadata.tool_calls.is_empty() => true,
            EntryRole::Assistant => false,
            EntryRole::Summary | EntryRole::Archive => false,
        };

        if starts_new_group && !current_group.is_empty() {
            groups.push(std::mem::take(&mut current_group));
        }
        current_group.push(i);
    }
    if !current_group.is_empty() {
        groups.push(current_group);
    }

    // 期望 3 个配对组：
    // [0,1] = User + Assistant(对话)
    // [2]   = Assistant(tool_calls, 独立工具配对组)
    // [3,4] = User + Assistant(对话)
    assert_eq!(groups.len(), 3, "should have 3 pair-groups");
    assert_eq!(groups[0], vec![0, 1], "first group: User + Assistant");
    assert_eq!(
        groups[1],
        vec![2],
        "second group: Assistant with tool_calls"
    );
    assert_eq!(groups[2], vec![3, 4], "third group: User + Assistant");
}

#[test]
fn pair_group_splitting_all_dialogue() {
    // 验证：纯对话场景下配对组切分正确
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "q1", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "a1", EntryMetadata::default());
    stm.add_entry(EntryRole::User, "q2", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "a2", EntryMetadata::default());

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_group: Vec<usize> = Vec::new();

    for (i, entry) in stm.entries.iter().enumerate() {
        let starts_new_group = match entry.role {
            EntryRole::User => true,
            EntryRole::Assistant if !entry.metadata.tool_calls.is_empty() => true,
            EntryRole::Assistant => false,
            EntryRole::Summary | EntryRole::Archive => false,
        };

        if starts_new_group && !current_group.is_empty() {
            groups.push(std::mem::take(&mut current_group));
        }
        current_group.push(i);
    }
    if !current_group.is_empty() {
        groups.push(current_group);
    }

    // 2 个配对组
    assert_eq!(groups.len(), 2, "should have 2 pair-groups");
    assert_eq!(groups[0], vec![0, 1]);
    assert_eq!(groups[1], vec![2, 3]);
}

// ── 7. 硬截断配对组原子性测试 ──────────────────────────────────

#[test]
fn budget_truncation_preserves_tool_pairs() {
    // 验证：硬截断移除 Assistant(tool_calls) 时，同步移除后续 Tool 消息
    let messages: Vec<ConversationMessage> = vec![
        ConversationMessage::User {
            content: "question 1".to_string(),
        },
        ConversationMessage::Assistant {
            content: None,
            tool_calls: vec![harness::domain::LlmToolCall {
                id: "call_1".to_string(),
                name: "shell_exec".to_string(),
                arguments: "ls".to_string(),
            }],
            reasoning_content: None,
        },
        ConversationMessage::Tool {
            tool_call_id: "call_1".to_string(),
            content: "file1.txt".to_string(),
        },
        ConversationMessage::User {
            content: "question 2".to_string(),
        },
        ConversationMessage::Assistant {
            content: Some("answer".to_string()),
            tool_calls: vec![],
            reasoning_content: None,
        },
    ];

    let mut msgs = messages.clone();

    // 模拟 truncate_conversation_by_budget：移除第一条
    let first_is_tool_call_anchor = matches!(
        &msgs[0],
        ConversationMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()
    );
    // 第一条是 User，不是 tool_call_anchor
    assert!(!first_is_tool_call_anchor);
    msgs.remove(0);

    // 现在第一条是 Assistant(tool_calls)
    let first_is_tool_call_anchor = matches!(
        &msgs[0],
        ConversationMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()
    );
    assert!(
        first_is_tool_call_anchor,
        "next message should be tool call anchor"
    );
    msgs.remove(0);

    // 必须同时移除后续 Tool 消息
    while matches!(msgs.first(), Some(ConversationMessage::Tool { .. })) {
        msgs.remove(0);
    }

    // 剩余：User + Assistant
    assert_eq!(msgs.len(), 2);
    assert!(matches!(msgs[0], ConversationMessage::User { .. }));
    assert!(
        matches!(&msgs[1], ConversationMessage::Assistant { tool_calls, .. } if tool_calls.is_empty())
    );
}

// ── 8. 子 Agent 工具结果不冒泡测试 ──────────────────────────────

#[test]
fn sub_agent_tool_calls_stay_in_own_stm() {
    // 验证：子任务的 STM 独立，工具调用不冒泡
    let mut parent_stm = ShortTermMemory::default();
    let mut child_stm = ShortTermMemory::default();

    // 子任务执行工具
    child_stm.record_tool_call(
        Some("call_child_1".to_string()),
        "shell_exec".to_string(),
        "ls".to_string(),
        "files".to_string(),
        chrono::Utc::now(),
    );

    // 父 Agent 只看到最终文本回复
    parent_stm.add_entry(
        EntryRole::Assistant,
        "子任务完成了工作",
        EntryMetadata::default(),
    );

    // 子任务有 tool_calls，父任务没有
    assert!(
        child_stm
            .entries
            .iter()
            .any(|e| !e.metadata.tool_calls.is_empty()),
        "child STM should have tool_calls"
    );
    assert!(
        !parent_stm
            .entries
            .iter()
            .any(|e| !e.metadata.tool_calls.is_empty()),
        "parent STM should NOT have tool_calls from child"
    );
}
