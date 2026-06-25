use std::sync::Arc;

use crossbeam_channel::Sender;
use rhai::Engine;

use crate::domain::ExperienceStore;
use crate::user_plugins::host_api::entity_write::WorldCommand;

#[derive(Clone)]
pub struct ExperienceContext {
    pub store: Arc<ExperienceStore>,
    pub tx: Sender<WorldCommand>,
}

pub fn register(engine: &mut Engine, ctx: ExperienceContext) {
    let c = ctx.clone();
    engine.register_fn(
        "experience_get_candidate",
        move |id: &str| -> rhai::Dynamic {
            match uuid::Uuid::parse_str(id) {
                Ok(u) => c
                    .store
                    .candidates
                    .get(&u)
                    .map(|cand| {
                        let mut m = rhai::Map::new();
                        m.insert("title".into(), rhai::Dynamic::from(cand.title.clone()));
                        m.insert(
                            "kind".into(),
                            rhai::Dynamic::from(format!("{:?}", cand.kind_hint)),
                        );
                        m.insert(
                            "status".into(),
                            rhai::Dynamic::from(format!("{:?}", cand.status)),
                        );
                        rhai::Dynamic::from(m)
                    })
                    .unwrap_or(rhai::Dynamic::UNIT),
                Err(_) => rhai::Dynamic::UNIT,
            }
        },
    );

    let c = ctx.clone();
    engine.register_fn("experience_set_pinned", move |id: &str, pinned: bool| {
        if let Ok(u) = uuid::Uuid::parse_str(id) {
            let _ =
                c.tx.send(WorldCommand::ExperienceSetPinned { id: u, pinned });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    #[test]
    fn unknown_candidate_returns_unit() {
        let (tx, _rx) = unbounded();
        let mut e = Engine::new();
        register(
            &mut e,
            ExperienceContext {
                store: Arc::new(ExperienceStore::default()),
                tx,
            },
        );
        let v: () = e
            .eval(r#"experience_get_candidate("00000000-0000-0000-0000-000000000000")"#)
            .unwrap();
        assert_eq!(v, ());
    }
}
