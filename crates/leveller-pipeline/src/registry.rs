//! The stage registry.
//!
//! Stages are held by name so a chain can be described as data — a list of
//! names and parameters from a command-line flag, a config file or the UI —
//! rather than as imports wired up in code.
//!
//! A registry is built rather than global, which is what lets the trained
//! denoise backend be present in one build and absent in another without every
//! caller learning about it.

use std::sync::Arc;

use crate::stage::Stage;

#[derive(Clone, Default)]
pub struct Registry {
    stages: Vec<Arc<dyn Stage>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a stage. Registration order is the chain's default order.
    ///
    /// # Panics
    /// If a stage of that name is already registered — two stages answering to
    /// one name is a wiring mistake, not a runtime condition.
    pub fn register(&mut self, stage: Arc<dyn Stage>) -> &mut Self {
        assert!(
            self.get(stage.name()).is_none(),
            "stage \"{}\" is already registered",
            stage.name()
        );
        self.stages.push(stage);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Stage>> {
        self.stages.iter().find(|s| s.name() == name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Every stage, in registration order.
    pub fn stages(&self) -> &[Arc<dyn Stage>] {
        &self.stages
    }

    /// Every stage's name, in registration order — which is chain order.
    pub fn names(&self) -> Vec<&'static str> {
        self.stages.iter().map(|s| s.name()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.stages.len()
    }
}
