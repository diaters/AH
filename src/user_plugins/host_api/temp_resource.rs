use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rhai::{Dynamic, Engine};

#[derive(Clone, Default)]
pub struct TempResourceSlot {
    pub inner: Arc<Mutex<HashMap<String, Dynamic>>>,
}

impl TempResourceSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drain(&self) -> HashMap<String, Dynamic> {
        let mut g = self.inner.lock().unwrap();
        std::mem::take(&mut *g)
    }
}

pub fn register(engine: &mut Engine, slot: TempResourceSlot) {
    let s = slot.clone();
    engine.register_fn(
        "register_temp_resource",
        move |key: &str, value: Dynamic| {
            let mut g = s.inner.lock().unwrap();
            g.insert(key.to_string(), value);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_temp_resource_stores_into_slot() {
        let slot = TempResourceSlot::new();
        let mut e = Engine::new();
        register(&mut e, slot.clone());
        e.eval::<()>(r#"register_temp_resource("k", "v")"#).unwrap();
        let drained = slot.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained["k"].clone().cast::<String>(), "v");
    }

    #[test]
    fn drain_empties_slot() {
        let slot = TempResourceSlot::new();
        let mut e = Engine::new();
        register(&mut e, slot.clone());
        e.eval::<()>(r#"register_temp_resource("k1", "v1")"#)
            .unwrap();
        let _ = slot.drain();
        assert!(slot.inner.lock().unwrap().is_empty());
    }
}
