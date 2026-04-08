use std::any::{Any, TypeId};
use std::collections::HashMap;

/// A TypeId-keyed map for attaching arbitrary typed metadata to requests.
pub struct Extensions {
    map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Extensions {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(value));
    }

    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref())
    }
}

impl Default for Extensions {
    fn default() -> Self {
        Self::new()
    }
}
