use std::sync::Arc;

use rhai::Engine;

use crate::domain::ExperienceStore;

#[derive(Clone)]
pub struct ExperienceContext {
    pub store: Arc<ExperienceStore>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_candidate_returns_unit() {
        let mut e = Engine::new();
        register(
            &mut e,
            ExperienceContext {
                store: Arc::new(ExperienceStore::default()),
            },
        );
        let v: () = e
            .eval(r#"experience_get_candidate("00000000-0000-0000-0000-000000000000")"#)
            .unwrap();
        assert_eq!(v, ());
    }
}
