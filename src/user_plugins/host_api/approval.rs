use crossbeam_channel::Sender;
use rhai::Engine;

use crate::user_plugins::host_api::entity_write::WorldCommand;

#[derive(Clone)]
pub struct ApprovalContext {
    pub current_request_id: Option<uuid::Uuid>,
    pub tx: Sender<WorldCommand>,
}

pub fn register(engine: &mut Engine, ctx: ApprovalContext) {
    let c = ctx.clone();
    engine.register_fn("approval_request_id", move || -> String {
        c.current_request_id
            .map(|u| u.to_string())
            .unwrap_or_default()
    });

    let c = ctx.clone();
    engine.register_fn(
        "approval_resolve",
        move |request_id: &str, decision: &str| {
            if let Ok(id) = uuid::Uuid::parse_str(request_id) {
                let _ = c.tx.send(WorldCommand::SetApprovalDecision {
                    request_id: id,
                    decision: decision.to_string(),
                });
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    #[test]
    fn approval_request_id_returns_empty_string_when_none() {
        let (tx, _rx) = unbounded();
        let mut e = Engine::new();
        register(
            &mut e,
            ApprovalContext {
                current_request_id: None,
                tx,
            },
        );
        let s: String = e.eval(r#"approval_request_id()"#).unwrap();
        assert_eq!(s, "");
    }
}
