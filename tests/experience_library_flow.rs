//! 经验库检索端到端：主 Agent 调用 search_experience → 经验管理员 WorkItem →
//! 按管理员选择逐字回填原文 → 主 Agent 继续完成任务。
//!
//! 重点验证：检索期间父任务停留在 Waiting(ToolExecution)，不会被管理员请求标记为 Running
//! 而卡住工具循环。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use crossbeam_channel::unbounded;
use harness::prelude::*;
use harness::{
    app::build_harness_app,
    contracts::ExperienceLibrary,
    domain::{
        Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest,
        AgentExecutionRequestMessage, AgentExecutor, AgentId, AgentKind, AgentProfile,
        AgentRequestKind, AgentToolPermissions, ChannelId, ConversationMessage, ExperienceEntry,
        FrontendKind, LlmToolCall, OutputContent, ShortTermMemory, SpaceToolRegistry, Task,
        TaskStatus, ToolPermission, WaitingReason,
    },
    infrastructure::experience::FileExperienceLibrary,
    llm::ExecutorRegistry,
    systems::{HarnessConfig, experience_library::ExperienceLibraryHandle},
};
use tokio::runtime::Runtime;

const BODY: &str = "## 适用场景\nhdc install 报错 9568322\n\n## 可行做法\n`hdc install -r signed.hap`\n\n## 走不通的路线\n- 直接重装未签名包：仍报 9568322\n\n## 前提\nDevEco 5.0";

struct Recorder {
    experience_id: String,
    librarian_prompts: Arc<Mutex<Vec<String>>>,
    tool_results: Arc<Mutex<Vec<String>>>,
}

impl AgentExecutor for Recorder {
    fn execute(&self, request: AgentExecutionRequest) -> harness::domain::ExecutorFuture {
        let text = |t: String| AgentExecutionOutput {
            content: OutputContent::Text(t),
            reasoning_content: None,
        };
        let output = match request.request_kind {
            AgentRequestKind::Experience => {
                self.librarian_prompts
                    .lock()
                    .unwrap()
                    .push(request.prompt.clone());
                text(format!(
                    r#"{{"matches":[{{"id":"{}","reason":"同一个安装报错"}}]}}"#,
                    self.experience_id
                ))
            }
            _ => match &request.conversation {
                Some(conversation) => {
                    for message in conversation {
                        if let ConversationMessage::Tool { content, .. } = message {
                            self.tool_results.lock().unwrap().push(content.clone());
                        }
                    }
                    text("已按经验重新签名后安装".into())
                }
                None => AgentExecutionOutput {
                    content: OutputContent::ToolCalls(vec![LlmToolCall {
                        id: "call_lookup".into(),
                        name: "search_experience".into(),
                        arguments: r#"{"situation":"hdc install 报错 9568322"}"#.into(),
                    }]),
                    reasoning_content: None,
                },
            },
        };
        Box::pin(async move { Ok(output) })
    }
}

/// 不读取工作目录下的 agents.toml，只使用用例内创建的 Agent。
fn isolated_config() -> HarnessConfig {
    HarnessConfig {
        approval_mode: harness::systems::ApprovalMode::Manual,
        agents_config_path: "/nonexistent_agents.toml".to_string(),
        ..HarnessConfig::default()
    }
}

fn spawn_agent(world: &mut World, name: &str, tags: &[&str]) -> AgentId {
    let id = AgentId::new();
    let entity = world
        .spawn(Agent {
            id,
            profile: AgentProfile {
                name: name.into(),
                model: "test-model".into(),
            },
            capabilities: AgentCapabilities {
                tags: tags.iter().map(|t| t.to_string()).collect(),
                description: name.into(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: HashMap::new(),
            },
            system_prompt: None,
        })
        .id();
    world
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(id, entity);
    id
}

#[test]
fn search_experience_returns_librarian_selected_entry_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let library = Arc::new(FileExperienceLibrary::new(dir.path()));
    let entry = ExperienceEntry::new("HAP 安装签名错误", BODY, None, chrono::Utc::now()).unwrap();
    library.save(&entry).unwrap();
    let unrelated = ExperienceEntry::new(
        "监控电源",
        "## 适用场景\n笔记本电源",
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    library.save(&unrelated).unwrap();

    let librarian_prompts = Arc::new(Mutex::new(Vec::new()));
    let tool_results = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<dyn AgentExecutor> = Arc::new(Recorder {
        experience_id: entry.id.clone(),
        librarian_prompts: librarian_prompts.clone(),
        tool_results: tool_results.clone(),
    });
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        isolated_config(),
        Arc::new(Runtime::new().unwrap()),
        ExecutorRegistry::from_single_executor(executor, "default"),
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    app.update();
    app.insert_resource(ExperienceLibraryHandle(library));

    let main_agent = spawn_agent(app.world_mut(), "main", &["general"]);
    spawn_agent(app.world_mut(), "librarian", &["experience-librarian"]);

    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".into(),
        thread_id: None,
    };
    let mut task = Task::from_user_input_ready("安装 HAP", 3, channel);
    task.mark_waiting(WaitingReason::Agent, chrono::Utc::now());
    // 不经用户会话创建：本用例只验证工具检索，不触发开局预查。
    task.origin_channel = None;
    let task_id = task.id;
    let task_entity = app
        .world_mut()
        .spawn((task, ShortTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    let tools: Vec<_> = app
        .world()
        .resource::<SpaceToolRegistry>()
        .iter()
        .filter(|t| t.name == "search_experience")
        .cloned()
        .collect();
    assert_eq!(tools.len(), 1, "search_experience must be registered");
    app.world_mut().spawn(AgentExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id: main_agent,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "安装 HAP".into(),
            system_prompt: None,
            tools,
            conversation: None,
            work_item_id: None,
            model_override: None,
            turn_generation: None,
            vision: false,
            trusted_images: Vec::new(),
        },
    });

    for _ in 0..30 {
        app.update();
        std::thread::sleep(Duration::from_millis(20));
        if app
            .world()
            .get::<Task>(task_entity)
            .is_some_and(|t| matches!(t.status(), TaskStatus::Done))
        {
            break;
        }
    }

    let status = app
        .world()
        .get::<Task>(task_entity)
        .unwrap()
        .status()
        .clone();
    assert!(
        matches!(status, TaskStatus::Done),
        "tool loop must resume after lookup, got {status:?}"
    );
    let prompts = librarian_prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1, "exactly one librarian call");
    assert!(prompts[0].contains(&entry.id) && prompts[0].contains(&unrelated.id));
    let results = tool_results.lock().unwrap();
    let result = results
        .first()
        .expect("tool result delivered to main agent");
    let body_json = serde_json::to_string(BODY).unwrap();
    assert!(
        result.contains(body_json.trim_matches('"')),
        "selected entry must be returned verbatim: {result}"
    );
    assert!(
        !result.contains("笔记本电源"),
        "unselected entry must not leak"
    );
}

// ============ 开局预查与起草 ============

type LibrarianReply = Box<dyn Fn(&str) -> String + Send + Sync>;

/// 管理员按脚本回复；主 Agent 记录收到的 prompt 并直接文本作答。
struct Scripted {
    librarian: LibrarianReply,
    librarian_prompts: Arc<Mutex<Vec<String>>>,
    main_prompts: Arc<Mutex<Vec<String>>>,
}

impl AgentExecutor for Scripted {
    fn execute(&self, request: AgentExecutionRequest) -> harness::domain::ExecutorFuture {
        let reply = if request.request_kind == AgentRequestKind::Experience {
            self.librarian_prompts
                .lock()
                .unwrap()
                .push(request.prompt.clone());
            (self.librarian)(&request.prompt)
        } else {
            self.main_prompts
                .lock()
                .unwrap()
                .push(request.prompt.clone());
            "好的".to_string()
        };
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text(reply),
                reasoning_content: None,
            })
        })
    }
}

#[derive(Clone, Default)]
struct CapturingClient(Arc<Mutex<Vec<harness::domain::EngineEvent>>>);

impl harness::domain::Frontend for CapturingClient {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Client
    }
    fn push_event(&self, event: harness::domain::EngineEvent) {
        self.0.lock().unwrap().push(event);
    }
    fn poll_actions(&self) -> Vec<harness::domain::UserAction> {
        vec![]
    }
    fn requires_polling(&self) -> bool {
        false
    }
}

struct Harness {
    app: bevy_app::App,
    input: crossbeam_channel::Sender<harness::domain::ExternalInput>,
    librarian_prompts: Arc<Mutex<Vec<String>>>,
    main_prompts: Arc<Mutex<Vec<String>>>,
    main_agent: AgentId,
}

fn harness(
    library: Arc<FileExperienceLibrary>,
    librarian: LibrarianReply,
    frontends: Vec<Box<dyn harness::domain::Frontend>>,
) -> Harness {
    let librarian_prompts = Arc::new(Mutex::new(Vec::new()));
    let main_prompts = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<dyn AgentExecutor> = Arc::new(Scripted {
        librarian,
        librarian_prompts: librarian_prompts.clone(),
        main_prompts: main_prompts.clone(),
    });
    let (input, input_rx) = unbounded();
    let mut app = build_harness_app(
        isolated_config(),
        Arc::new(Runtime::new().unwrap()),
        ExecutorRegistry::from_single_executor(executor, "default"),
        input_rx,
        frontends,
        harness::channels::ChannelManager::empty().0,
    );
    app.update();
    app.insert_resource(ExperienceLibraryHandle(library));
    let main_agent = spawn_agent(app.world_mut(), "main", &["general"]);
    spawn_agent(app.world_mut(), "librarian", &["experience-librarian"]);
    Harness {
        app,
        input,
        librarian_prompts,
        main_prompts,
        main_agent,
    }
}

fn library_with_entry() -> (
    tempfile::TempDir,
    Arc<FileExperienceLibrary>,
    ExperienceEntry,
) {
    let dir = tempfile::tempdir().unwrap();
    let library = Arc::new(FileExperienceLibrary::new(dir.path()));
    let entry = ExperienceEntry::new("HAP 安装签名错误", BODY, None, chrono::Utc::now()).unwrap();
    library.save(&entry).unwrap();
    (dir, library, entry)
}

fn spawn_user_task(
    app: &mut bevy_app::App,
    status: WaitingReason,
) -> (Entity, harness::domain::TaskId) {
    let mut task = Task::from_user_input_ready(
        "hdc install 一直报 9568322",
        3,
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".into(),
            thread_id: None,
        },
    );
    task.mark_waiting(status, chrono::Utc::now());
    let task_id = task.id;
    let entity = app
        .world_mut()
        .spawn((task, ShortTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, entity);
    (entity, task_id)
}

fn pump(app: &mut bevy_app::App, mut done: impl FnMut(&mut bevy_app::App) -> bool) -> bool {
    for _ in 0..40 {
        app.update();
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn has_hint(app: &bevy_app::App, entity: Entity) -> bool {
    app.world()
        .get::<harness::domain::TaskInbox>(entity)
        .is_some_and(|inbox| {
            inbox
                .items
                .iter()
                .any(|i| matches!(i, harness::domain::InboxItem::ExperienceHint { .. }))
        })
}

#[test]
fn preflight_hint_reaches_next_main_agent_request() {
    let (_dir, library, entry) = library_with_entry();
    let id = entry.id.clone();
    let mut h = harness(
        library,
        Box::new(move |_| format!(r#"{{"matches":[{{"id":"{id}","reason":"同一报错"}}]}}"#)),
        vec![],
    );
    let (entity, task_id) = spawn_user_task(&mut h.app, WaitingReason::Agent);
    assert!(
        pump(&mut h.app, |app| has_hint(app, entity)),
        "preflight hint delivered"
    );
    assert!(h.librarian_prompts.lock().unwrap()[0].contains("9568322"));

    h.app.world_mut().spawn(AgentExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id: h.main_agent,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "hdc install 一直报 9568322".into(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: None,
            turn_generation: None,
            vision: false,
            trusted_images: Vec::new(),
        },
    });
    let prompts = h.main_prompts.clone();
    assert!(pump(&mut h.app, |_| !prompts.lock().unwrap().is_empty()));
    let prompt = prompts.lock().unwrap()[0].clone();
    assert!(prompt.contains("<experience_hint>"), "{prompt}");
    assert!(prompt.contains(BODY), "hint must carry the entry verbatim");
    let done = pump(&mut h.app, |app| {
        app.world()
            .get::<Task>(entity)
            .is_some_and(|t| matches!(t.status(), TaskStatus::Done))
    });
    let status = h.app.world().get::<Task>(entity).unwrap().status().clone();
    assert!(done, "task must complete after the hinted turn: {status:?}");
}

#[test]
fn preflight_hint_dropped_when_task_already_waits_for_user() {
    let (_dir, library, entry) = library_with_entry();
    let id = entry.id.clone();
    let mut h = harness(
        library,
        Box::new(move |_| format!(r#"{{"matches":[{{"id":"{id}","reason":"x"}}]}}"#)),
        vec![],
    );
    let (entity, _) = spawn_user_task(&mut h.app, WaitingReason::User);
    let prompts = h.librarian_prompts.clone();
    assert!(pump(&mut h.app, |_| !prompts.lock().unwrap().is_empty()));
    for _ in 0..5 {
        h.app.update();
    }
    assert!(
        !has_hint(&h.app, entity),
        "idle task must not receive a stale hint"
    );
}

#[test]
fn preflight_skips_librarian_when_library_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(
        Arc::new(FileExperienceLibrary::new(dir.path())),
        Box::new(|_| r#"{"matches":[]}"#.into()),
        vec![],
    );
    spawn_user_task(&mut h.app, WaitingReason::Agent);
    for _ in 0..10 {
        h.app.update();
    }
    assert!(h.librarian_prompts.lock().unwrap().is_empty());
}

#[test]
fn draft_request_produces_client_event_with_merge_target() {
    let (_dir, library, entry) = library_with_entry();
    let target = entry.id.clone();
    let client = CapturingClient::default();
    let mut h = harness(
        library,
        Box::new(move |_| {
            serde_json::json!({"title": "HAP 安装签名错误", "body": "## 适用场景\n更新", "merge_target": target})
                .to_string()
        }),
        vec![Box::new(client.clone())],
    );
    let draft_id = uuid::Uuid::new_v4();
    h.input
        .send(harness::domain::ExternalInput::ExperienceDraft {
            channel: ChannelId {
                frontend: FrontendKind::Client,
                user_id: "u".into(),
                thread_id: Some(uuid::Uuid::new_v4().to_string()),
            },
            draft_id,
            transcript: "[用户] hdc install 报 9568322\n[失败 shell_exec] 9568322".into(),
            note: Some("记下签名步骤".into()),
        })
        .unwrap();
    let is_draft = |e: &harness::domain::EngineEvent| {
        matches!(
            e,
            harness::domain::EngineEvent::ExperienceDraftUpdated { .. }
        )
    };
    let events = client.0.clone();
    assert!(pump(&mut h.app, |_| events
        .lock()
        .unwrap()
        .iter()
        .any(is_draft)));
    let prompt = h.librarian_prompts.lock().unwrap()[0].clone();
    assert!(prompt.contains("记下签名步骤") && prompt.contains(&entry.id));
    let events = events.lock().unwrap();
    let Some(harness::domain::EngineEvent::ExperienceDraftUpdated {
        draft_id: got,
        outcome: Ok(draft),
        ..
    }) = events.iter().find(|e| is_draft(e))
    else {
        panic!("expected successful draft event, got {events:?}");
    };
    assert_eq!(*got, draft_id);
    assert_eq!(draft.merge_target.as_deref(), Some(entry.id.as_str()));
}
