//! 插件对 World 的写指令。dispatcher 在 hook 完成后 replay。
//!
//! 注意：本枚举在后续任务会继续扩展（CreateWorkItem、SetApprovalDecision、
//! ExperienceSetPinned、SetTaskTag 等）。每个新增变体都要同步追加到 `replay`
//! 函数匹配分支。

use crossbeam_channel::Sender;
use rhai::Engine;
use uuid::Uuid;

/// 插件对 World 的写指令。dispatcher 在 hook 完成后 replay。
///
/// 注意：本枚举在后续任务会继续扩展（CreateWorkItem、SetApprovalDecision、
/// ExperienceSetPinned、SetTaskTag 等）。每个新增变体都要同步追加到 `replay`
/// 函数匹配分支。
#[derive(Debug)]
pub enum WorldCommand {
    CreateTask {
        title: String,
        parent: Option<Uuid>,
    },
    SetTaskMetadata {
        task_id: Uuid,
        key: String,
        value: String,
    },
    SetTaskTag {
        task_id: Uuid,
        key: String,
        value: String,
    },
    CreateWorkItem {
        task_id: Uuid,
        kind: String,
        payload: serde_json::Value,
    },
    SetApprovalDecision {
        request_id: Uuid,
        decision: String,
    },
    ExperienceSetPinned {
        id: Uuid,
        pinned: bool,
    },
}

/// 每个 hook 派发携带的 sender。
#[derive(Clone)]
pub struct WorldWriter {
    pub tx: Sender<WorldCommand>,
}

impl WorldWriter {
    pub fn new(tx: Sender<WorldCommand>) -> Self {
        Self { tx }
    }
}

pub fn register(engine: &mut Engine, writer: WorldWriter) {
    let w = writer.clone();
    engine.register_fn("create_task", move |title: &str| -> String {
        let cmd = WorldCommand::CreateTask {
            title: title.to_string(),
            parent: None,
        };
        let _ = w.tx.send(cmd);
        // 临时返回占位 uuid，真实 id 在 dispatcher replay 后写入。
        // hook 若需要立即拿 id 应改为后 hook / on_task_created。
        uuid::Uuid::nil().to_string()
    });

    let w = writer.clone();
    engine.register_fn(
        "task_set_metadata",
        move |task_id: &str, key: &str, value: &str| {
            if let Ok(id) = Uuid::parse_str(task_id) {
                let _ = w.tx.send(WorldCommand::SetTaskMetadata {
                    task_id: id,
                    key: key.to_string(),
                    value: value.to_string(),
                });
            }
        },
    );

    let w = writer.clone();
    engine.register_fn(
        "task_set_tag",
        move |task_id: &str, key: &str, value: &str| {
            if let Ok(id) = Uuid::parse_str(task_id) {
                let _ = w.tx.send(WorldCommand::SetTaskTag {
                    task_id: id,
                    key: key.to_string(),
                    value: value.to_string(),
                });
            }
        },
    );

    let w = writer.clone();
    engine.register_fn(
        "create_work_item",
        move |task_id: &str, kind: &str, payload: rhai::Dynamic| -> String {
            if let Ok(tid) = Uuid::parse_str(task_id) {
                let payload_json = rhai_dynamic_to_json(payload);
                let _ = w.tx.send(WorldCommand::CreateWorkItem {
                    task_id: tid,
                    kind: kind.to_string(),
                    payload: payload_json,
                });
            }
            uuid::Uuid::nil().to_string()
        },
    );
}

pub fn rhai_dynamic_to_json(v: rhai::Dynamic) -> serde_json::Value {
    if v.is_unit() {
        return serde_json::Value::Null;
    }
    if v.is::<bool>() {
        return serde_json::Value::Bool(v.as_bool().unwrap());
    }
    if v.is::<i64>() {
        return serde_json::Value::from(v.as_int().unwrap());
    }
    if v.is::<f64>() {
        return serde_json::json!(v.as_float().unwrap());
    }
    if v.is::<String>() {
        return serde_json::Value::String(v.cast::<String>());
    }
    if v.is::<rhai::Map>() {
        let m = v.cast::<rhai::Map>();
        let mut obj = serde_json::Map::new();
        for (k, val) in m.iter() {
            obj.insert(k.to_string(), rhai_dynamic_to_json(val.clone()));
        }
        return serde_json::Value::Object(obj);
    }
    if v.is::<rhai::Array>() {
        let arr = v.cast::<rhai::Array>();
        return serde_json::Value::Array(
            arr.iter()
                .map(|v| rhai_dynamic_to_json(v.clone()))
                .collect(),
        );
    }
    serde_json::Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    #[test]
    fn create_task_sends_command() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, WorldWriter::new(tx));
        let _ = e.eval::<String>(r#"create_task("hello")"#).unwrap();
        let cmd = rx.recv().unwrap();
        match cmd {
            WorldCommand::CreateTask { title, .. } => assert_eq!(title, "hello"),
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn task_set_metadata_with_bad_uuid_sends_nothing() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, WorldWriter::new(tx));
        e.eval::<()>(r#"task_set_metadata("not-uuid", "k", "v")"#)
            .unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn task_set_tag_sends_command() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, WorldWriter::new(tx));
        let id = uuid::Uuid::new_v4();
        let script = format!(r#"task_set_tag("{}", "env", "ci")"#, id);
        e.eval::<()>(&script).unwrap();
        match rx.recv().unwrap() {
            WorldCommand::SetTaskTag {
                task_id,
                key,
                value,
            } => {
                assert_eq!(task_id, id);
                assert_eq!(key, "env");
                assert_eq!(value, "ci");
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn create_work_item_sends_payload_command() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, WorldWriter::new(tx));
        let tid = uuid::Uuid::new_v4();
        let script = format!(
            r#"
let p = #{{"topic": "ci-fail", "severity": 5}};
create_work_item("{}", "triage", p)
"#,
            tid
        );
        let ret: String = e.eval(&script).unwrap();
        assert_eq!(ret, uuid::Uuid::nil().to_string());
        match rx.recv().unwrap() {
            WorldCommand::CreateWorkItem {
                task_id,
                kind,
                payload,
            } => {
                assert_eq!(task_id, tid);
                assert_eq!(kind, "triage");
                assert_eq!(payload["topic"], "ci-fail");
                assert_eq!(payload["severity"], 5);
            }
            _ => panic!("wrong cmd"),
        }
    }
}
