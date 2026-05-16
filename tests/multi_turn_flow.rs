use std::sync::Arc;

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    build_harness_app, Agent, AgentCapabilities, AgentExecutor, AgentExecutionRequest,
    AgentKind, AgentProfile, ExecutorFuture, HarnessConfig, LongTermMemory,
    OutputMessage, ShortTermMemory, Task, TaskStatus, WaitingReason,
};
use tokio::runtime::Runtime;

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move { Ok("echo response".to_string()) })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

#[test]
fn multi_turn_task_lifecycle() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // Create a task in Waiting(User) state
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "multi-turn task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            input_summary: "multi-turn task".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
        },
        ShortTermMemory {
            entries: vec![],
            turn_count: 1,
            summary_prefix: None,
            summary_range: None,
            last_cached_tokens: None,
        },
    ));

    // Simulate user input
    app.world_mut().spawn(harness::UserInputMessage {
        content: "continue with this input".to_string(),
    });

    // Run several frames
    for _ in 0..10 {
        app.update();
    }

    // Verify task state change
    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .cloned();

    assert!(task.is_some());
    let task = task.unwrap();
    // Task should have left Waiting(User) state
    assert_ne!(
        task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "task should have left Waiting(User) state"
    );
}

#[test]
fn short_term_memory_tracks_turns() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // Create a task with short-term memory
    let task_id = uuid::Uuid::new_v4();
    let entity_id = {
        let entity = app.world_mut().spawn((
            Task {
                id: task_id,
                content: "test".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Running,
                input_summary: "test".to_string(),
                result_summary: String::new(),
                priority: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
            },
            ShortTermMemory::default(),
        ));
        entity.id()
    };

    // Add entries to the short-term memory
    {
        let mut stm = app
            .world_mut()
            .get_mut::<ShortTermMemory>(entity_id)
            .unwrap();
        stm.add_entry(harness::EntryRole::User, "hello", Default::default());
        stm.add_entry(
            harness::EntryRole::Assistant,
            "hi there",
            Default::default(),
        );
    }

    app.update();

    // Verify memory entries
    let stored = app
        .world_mut()
        .query::<&ShortTermMemory>()
        .iter(app.world())
        .find(|_| true);

    assert!(stored.is_some());
    let stored = stored.unwrap();
    assert_eq!(stored.turn_count, 2);
    assert_eq!(stored.entries.len(), 2);
}

#[test]
fn agent_has_long_term_memory() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // Run one frame to initialize the app and load persistent agents from config
    app.update();

    // Spawn a persistent agent manually
    let agent_id = uuid::Uuid::new_v4();
    app.world_mut().spawn(Agent {
        id: agent_id,
        profile: AgentProfile {
            name: "test-agent".to_string(),
            model: "gpt-4".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["test".to_string()],
            description: "test agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
    });

    // Run another frame to trigger init_agent_memory_system for the new agent
    app.update();

    // Verify the newly spawned agent has long-term memory
    let has_memory = app
        .world_mut()
        .query::<(&Agent, &LongTermMemory)>()
        .iter(app.world())
        .any(|(a, _)| a.id == agent_id);

    assert!(
        has_memory,
        "the spawned agent should have long-term memory after init_agent_memory_system runs"
    );
}

#[test]
fn memory_contribution_on_agent_termination() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // Initialize the app first
    app.update();

    // Create parent agent with memory
    let parent_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: parent_id,
            profile: AgentProfile {
                name: "parent".to_string(),
                model: "gpt-4".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["general".to_string()],
                description: "parent agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
        },
        LongTermMemory::default(),
    ));

    // Create child task-scoped agent with memory
    let child_id = uuid::Uuid::new_v4();
    let task_id = uuid::Uuid::new_v4();
    let child_entity_id = {
        let entity = app.world_mut().spawn((
            Agent {
                id: child_id,
                profile: AgentProfile {
                    name: "child".to_string(),
                    model: "gpt-4".to_string(),
                },
                capabilities: AgentCapabilities {
                    tags: vec!["general".to_string()],
                    description: "child agent".to_string(),
                },
                kind: AgentKind::TaskScoped,
                parent_id: Some(parent_id),
                bound_task_id: Some(task_id),
            },
            LongTermMemory::default(),
        ));
        entity.id()
    };

    // Add some memory to the child agent
    {
        let mut long_memory = app
            .world_mut()
            .get_mut::<LongTermMemory>(child_entity_id)
            .unwrap();
        long_memory.add_archive("learned something important");
    }

    // Create a task for the terminated message to reference
    app.world_mut().spawn(Task {
        id: task_id,
        content: "test task".to_string(),
        creator: parent_id,
        delegate: Some(child_id),
        status: TaskStatus::Done,
        input_summary: "test".to_string(),
        result_summary: "completed".to_string(),
        priority: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
    });

    // Trigger termination by spawning TaskTerminatedMessage
    app.world_mut().spawn(harness::TaskTerminatedMessage { task_id });

    // Run frames to allow systems to process
    for _ in 0..10 {
        app.update();
    }

    // Verify that either:
    // 1. MemoryContributionRequestMessage was generated, or
    // 2. MemoryAbsorptionMessage was generated (contribution processed), or
    // 3. Child agent was despawned and memory was absorbed
    let contribution_requests = app
        .world_mut()
        .query::<&harness::MemoryContributionRequestMessage>()
        .iter(app.world())
        .count();

    let absorption_messages = app
        .world_mut()
        .query::<&harness::MemoryAbsorptionMessage>()
        .iter(app.world())
        .count();

    // Check if child agent still exists
    let child_exists = app
        .world_mut()
        .query::<&Agent>()
        .iter(app.world())
        .any(|a| a.id == child_id);

    // Check if parent has absorbed memory
    let parent_memory = app
        .world_mut()
        .query::<(&Agent, &LongTermMemory)>()
        .iter(app.world())
        .find(|(a, _)| a.id == parent_id)
        .map(|(_, m)| m.entries.len());

    // At least one of these should indicate the contribution flow worked
    assert!(
        contribution_requests > 0
            || absorption_messages > 0
            || !child_exists
            || parent_memory.map_or(false, |len| len > 0),
        "contribution flow should have processed: requests={}, absorptions={}, child_exists={}, parent_memory={:?}",
        contribution_requests,
        absorption_messages,
        child_exists,
        parent_memory
    );
}
