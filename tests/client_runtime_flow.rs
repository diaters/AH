mod common;

use harness::{
    app::build_harness_app,
    channels::{
        ChannelManager,
        client::{Client, ClientConfig, ClientServer, Store},
    },
    domain::{
        Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, LongTermMemory,
    },
    ecs::EntityIndex,
    llm::ExecutorRegistry,
    systems::{BrainConfig, HarnessConfig},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// HTTPS 命令 → 统一输入 → Brain/Agent（mock provider）→ 原生事件 → HTTPS 快照。
/// 不使用真实 provider，也不把 mock 回复当作真机或真实模型验收。
#[test]
fn paired_client_drives_unmodified_runtime_and_receives_agent_reply() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let mut store = Store::open(dir.path().join("client.json")).unwrap();
    let now = chrono::Utc::now().timestamp();
    let code = store.issue_pairing("alice", now).unwrap();
    let (_, token) = store.pair(&code, "test client", now).unwrap();
    let session = store
        .create_session(
            "alice",
            Uuid::new_v4(),
            "runtime flow",
            "create-test-session",
        )
        .unwrap()
        .session_id;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (tx, rx) = crossbeam_channel::unbounded();
    let client = Client::new(
        ClientConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            instance_name: "integration test".into(),
            tls_cert: Some(root.join("tests/fixtures/client-tls/test-cert.pem")),
            tls_key: Some(root.join("tests/fixtures/client-tls/test-key.pem")),
            advertise: vec![],
            state_path: dir.path().join("client.json"),
            meow: None,
        },
        store,
        tx.clone(),
    );
    let server = runtime
        .block_on(ClientServer::start(client.clone()))
        .unwrap();
    let http = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(include_bytes!("fixtures/client-tls/test-cert.pem"))
                .unwrap(),
        )
        .build()
        .unwrap();
    let url = format!("https://localhost:{}", server.address.port());
    let executor = Arc::new(common::mock_executor::BrainAwareEchoExecutor::new(
        "client runtime reply",
    ));
    let (manager, channel_task, frontends) = {
        let _guard = runtime.enter();
        ChannelManager::new(vec![Arc::new(client)], tx).unwrap()
    };
    let config = HarnessConfig {
        agents_config_path: dir
            .path()
            .join("no-agents.toml")
            .to_string_lossy()
            .into_owned(),
        brain: Some(BrainConfig {
            enabled: true,
            fallback_agent: None,
        }),
        ..Default::default()
    };
    let mut app = build_harness_app(
        config,
        runtime.clone(),
        ExecutorRegistry::from_single_executor(executor, "default"),
        rx,
        frontends,
        manager,
    );
    let schedule_store = harness::infrastructure::schedules::ScheduleStore::new(
        dir.path().join("schedules/schedules.json"),
    );
    app.insert_resource(schedule_store.clone());
    app.update();
    for (name, tags) in [
        ("brain", vec!["brain"]),
        ("default-llm-agent", vec!["default", "llm"]),
    ] {
        let agent = Agent {
            id: harness::domain::AgentId::new(),
            profile: AgentProfile {
                name: name.into(),
                model: "mock".into(),
            },
            capabilities: AgentCapabilities {
                tags: tags.into_iter().map(String::from).collect(),
                description: name.into(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        };
        let id = agent.id;
        let entity = app
            .world_mut()
            .spawn((agent, LongTermMemory::default()))
            .id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .agents
            .insert(id, entity);
    }
    let command = json!({"operation":"send_text","command_id":Uuid::new_v4(),"session_id":session,"content":"test native client path"});
    let receipt: Value = runtime.block_on(async {
        http.post(format!("{url}/v1/commands"))
            .bearer_auth(&token)
            .json(&command)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    });
    assert_eq!(receipt["status"], "queued");
    let deadline = Instant::now() + Duration::from_secs(5);
    let snapshot = loop {
        app.update();
        let snapshot: Value = runtime.block_on(async {
            http.get(format!("{url}/v1/snapshot"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        if snapshot["sessions"][0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "agent" && m["content"] == "client runtime reply")
        {
            break snapshot;
        }
        assert!(Instant::now() < deadline, "no runtime reply: {snapshot}");
        std::thread::sleep(Duration::from_millis(5));
    };
    let task = snapshot["sessions"][0]["tasks"]
        .as_object()
        .unwrap()
        .values()
        .find(|t| t["can_finish"] == true)
        .unwrap();
    let command_id = Uuid::new_v4();
    let finish = json!({"operation":"finish_task","command_id":command_id,"session_id":session,
        "task_id":task["task_id"],"expected_revision":task["revision"]});
    let response = runtime
        .block_on(
            http.post(format!("{url}/v1/commands"))
                .bearer_auth(&token)
                .json(&finish)
                .send(),
        )
        .unwrap();
    assert!(response.status().is_success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.update();
        let receipt: Value = runtime.block_on(async {
            http.get(format!("{url}/v1/commands/{command_id}"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        if receipt["status"] == "processed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "control was not processed: {receipt}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut stale = finish;
    stale["command_id"] = Uuid::new_v4().to_string().into();
    let response = runtime
        .block_on(
            http.post(format!("{url}/v1/commands"))
                .bearer_auth(&token)
                .json(&stale)
                .send(),
        )
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::CONFLICT,
        "old snapshot cannot control ended task"
    );

    // 同一 HTTPS 入口驱动真实调度器修改与磁盘落账，非直接调用管理函数。
    let schedule_id = Uuid::new_v4();
    let create = json!({"operation":"schedule","command_id":schedule_id,"session_id":session,
        "action":{"kind":"create","input":{"name":"client-owned","content":"check later","schedule":"cron:0 8 * * *","enabled":true,"notify":"reply"}}});
    let response = runtime
        .block_on(
            http.post(format!("{url}/v1/commands"))
                .bearer_auth(&token)
                .json(&create)
                .send(),
        )
        .unwrap();
    assert!(response.status().is_success());
    let deadline = Instant::now() + Duration::from_secs(5);
    let schedule = loop {
        app.update();
        let snapshot: Value = runtime.block_on(async {
            http.get(format!("{url}/v1/snapshot"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        if let Some(plan) = snapshot["automation"]["schedules"]
            .as_array()
            .and_then(|plans| plans.iter().find(|p| p["id"] == schedule_id.to_string()))
        {
            break plan.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no schedule snapshot: {snapshot}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(schedule_store.path().is_file());
    let mut input = create["action"]["input"].clone();
    input["enabled"] = false.into();
    let update_id = Uuid::new_v4();
    let update = json!({"operation":"schedule","command_id":update_id,"session_id":session,"action":{"kind":"update","schedule_id":schedule_id,"expected_revision":schedule["revision"],"input":input}});
    assert!(
        runtime
            .block_on(
                http.post(format!("{url}/v1/commands"))
                    .bearer_auth(&token)
                    .json(&update)
                    .send()
            )
            .unwrap()
            .status()
            .is_success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.update();
        let receipt: Value = runtime.block_on(async {
            http.get(format!("{url}/v1/commands/{update_id}"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        if receipt["status"] == "processed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "schedule update not processed: {receipt}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let disk: Value =
        serde_json::from_slice(&std::fs::read(schedule_store.path()).unwrap()).unwrap();
    let def = disk["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == schedule_id.to_string())
        .unwrap();
    assert_eq!(def["enabled"], false);
    assert_eq!(def["output_channel"]["user_id"], "alice");
    app.world().resource::<ChannelManager>().shutdown();
    runtime.block_on(channel_task).unwrap();
    runtime.block_on(server.shutdown()).unwrap();
}
