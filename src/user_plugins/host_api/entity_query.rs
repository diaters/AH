//! Host API：实体查询（get_task / get_work_item / get_agent 等）。
//!
//! dispatcher 在派发 hook 前从 World 拷贝快照注入，host 函数仅读快照，不访问 World。

use std::sync::Arc;

use crate::prelude::World;
use rhai::{Dynamic, Engine, Map};
use uuid::Uuid;

use crate::domain::{Agent, Task, WorkItem};

/// 派发 hook 时注入 Engine 的共享世界快照。
///
/// World 不能跨 await / 跨线程借用，dispatcher 在派发前把需要的快照
/// 拷贝到 `WorldSnapshot`，host API 只读此快照。
#[derive(Clone)]
pub struct WorldSnapshot {
    pub tasks: Arc<Vec<Task>>,
    pub work_items: Arc<Vec<WorkItem>>,
    pub agents: Arc<Vec<Agent>>,
}

impl WorldSnapshot {
    pub fn empty() -> Self {
        Self {
            tasks: Arc::new(Vec::new()),
            work_items: Arc::new(Vec::new()),
            agents: Arc::new(Vec::new()),
        }
    }

    pub fn from_world(world: &mut World) -> Self {
        let mut tasks: Vec<Task> = Vec::new();
        for task in world.query::<&Task>().iter(world) {
            tasks.push(task.clone());
        }
        let mut work_items: Vec<WorkItem> = Vec::new();
        for w in world.query::<&WorkItem>().iter(world) {
            work_items.push(w.clone());
        }
        let mut agents: Vec<Agent> = Vec::new();
        for a in world.query::<&Agent>().iter(world) {
            agents.push(a.clone());
        }
        Self {
            tasks: Arc::new(tasks),
            work_items: Arc::new(work_items),
            agents: Arc::new(agents),
        }
    }
}

pub fn register(engine: &mut Engine, snapshot: WorldSnapshot) {
    let snap = snapshot.clone();
    engine.register_fn("get_task", move |id: &str| -> Dynamic {
        match Uuid::parse_str(id) {
            Ok(uuid) => snap
                .tasks
                .iter()
                .find(|t| t.id == uuid)
                .map(task_to_map)
                .map(Dynamic::from)
                .unwrap_or(Dynamic::UNIT),
            Err(_) => Dynamic::UNIT,
        }
    });

    let snap = snapshot.clone();
    engine.register_fn("get_task_ids", move || -> Vec<String> {
        snap.tasks.iter().map(|t| t.id.to_string()).collect()
    });

    let snap = snapshot.clone();
    engine.register_fn("get_work_item", move |id: &str| -> Dynamic {
        match Uuid::parse_str(id) {
            Ok(uuid) => snap
                .work_items
                .iter()
                .find(|w| w.id == uuid)
                .map(work_item_to_map)
                .map(Dynamic::from)
                .unwrap_or(Dynamic::UNIT),
            Err(_) => Dynamic::UNIT,
        }
    });

    let snap = snapshot.clone();
    engine.register_fn(
        "get_work_item_ids_for",
        move |task_id: &str| -> Vec<String> {
            match Uuid::parse_str(task_id) {
                Ok(tid) => snap
                    .work_items
                    .iter()
                    .filter(|w| w.task_id == tid)
                    .map(|w| w.id.to_string())
                    .collect(),
                Err(_) => Vec::new(),
            }
        },
    );

    let snap = snapshot.clone();
    engine.register_fn("get_agent", move |id: &str| -> Dynamic {
        let needle = id.trim();
        snap.agents
            .iter()
            .find(|a| a.id.to_string() == needle)
            .map(agent_to_map)
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT)
    });

    let snap = snapshot.clone();
    engine.register_fn("get_agent_ids", move || -> Vec<String> {
        snap.agents.iter().map(|a| a.id.to_string()).collect()
    });
}

fn task_to_map(task: &Task) -> Map {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(task.id.to_string()));
    m.insert("content".into(), Dynamic::from(task.content.clone()));
    m.insert("status".into(), Dynamic::from(format!("{:?}", task.status)));
    m
}

fn work_item_to_map(w: &WorkItem) -> Map {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(w.id.to_string()));
    m.insert("task_id".into(), Dynamic::from(w.task_id.to_string()));
    m.insert("status".into(), Dynamic::from(format!("{:?}", w.status)));
    m
}

fn agent_to_map(a: &Agent) -> Map {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(a.id.to_string()));
    m.insert("name".into(), Dynamic::from(a.profile.name.clone()));
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ChannelId,
        FrontendKind, Task, WorkItem,
    };

    fn make_task(content: &str) -> Task {
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let mut t = Task::from_user_input(content.to_string(), 0, channel);
        t.id = uuid::Uuid::new_v4();
        t
    }

    fn make_agent(name: &str) -> Agent {
        Agent {
            id: uuid::Uuid::new_v4(),
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: Vec::new(),
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    #[test]
    fn get_task_returns_map_for_known_id() {
        let t = make_task("hello");
        let snap = WorldSnapshot {
            tasks: Arc::new(vec![t.clone()]),
            work_items: Arc::new(Vec::new()),
            agents: Arc::new(Vec::new()),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let script = format!(r#"let t = get_task("{}"); t.content"#, t.id);
        let out: String = e.eval(&script).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn get_task_ids_lists_all() {
        let snap = WorldSnapshot {
            tasks: Arc::new(vec![make_task("a"), make_task("b")]),
            work_items: Arc::new(Vec::new()),
            agents: Arc::new(Vec::new()),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let ids: Vec<String> = e.eval("get_task_ids()").unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn bad_uuid_returns_unit() {
        let snap = WorldSnapshot::empty();
        let mut e = Engine::new();
        register(&mut e, snap);
        let v: () = e.eval(r#"get_task("not-a-uuid")"#).unwrap();
        assert_eq!(v, ());
    }

    #[test]
    fn get_work_item_ids_for_filters_by_task() {
        let tid = uuid::Uuid::new_v4();
        let w = WorkItem::execution(tid, "do thing".to_string());
        let snap = WorldSnapshot {
            tasks: Arc::new(Vec::new()),
            work_items: Arc::new(vec![w]),
            agents: Arc::new(Vec::new()),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let script = format!(r#"get_work_item_ids_for("{}")"#, tid);
        let ids: Vec<String> = e.eval(&script).unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn get_agent_returns_name() {
        let a = make_agent("brain");
        let id_str = a.id.to_string();
        let snap = WorldSnapshot {
            tasks: Arc::new(Vec::new()),
            work_items: Arc::new(Vec::new()),
            agents: Arc::new(vec![a]),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let script = format!(r#"let a = get_agent("{}"); a.name"#, id_str);
        let out: String = e.eval(&script).unwrap();
        assert_eq!(out, "brain");
    }
}
