use rhai::Engine;

#[derive(Clone)]
pub struct ApprovalContext {
    pub current_request_id: Option<uuid::Uuid>,
}

pub fn register(engine: &mut Engine, ctx: ApprovalContext) {
    let c = ctx.clone();
    engine.register_fn("approval_request_id", move || -> String {
        c.current_request_id
            .map(|u| u.to_string())
            .unwrap_or_default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_request_id_returns_empty_string_when_none() {
        let mut e = Engine::new();
        register(
            &mut e,
            ApprovalContext {
                current_request_id: None,
            },
        );
        let s: String = e.eval(r#"approval_request_id()"#).unwrap();
        assert_eq!(s, "");
    }
}
