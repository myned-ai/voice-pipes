//! STT provider registry and capability-based dispatch.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::stt::{SttCapability, SttError, SttProvider};

/// A registry of named STT providers that supports runtime switching.
pub struct SttProviderRegistry {
    providers: HashMap<String, Arc<dyn SttProvider>>,
    active: String,
}

impl std::fmt::Debug for SttProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttProviderRegistry")
            .field("active", &self.active)
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SttProviderRegistry {
    /// Create a new registry with the given provider set as active.
    pub fn new(name: &str, provider: Arc<dyn SttProvider>) -> Self {
        let mut providers = HashMap::new();
        providers.insert(name.to_string(), provider);
        Self {
            providers,
            active: name.to_string(),
        }
    }

    /// Register an additional provider.
    pub fn register(&mut self, name: &str, provider: Arc<dyn SttProvider>) {
        self.providers.insert(name.to_string(), provider);
    }

    /// Get the currently active provider.
    pub fn active(&self) -> &Arc<dyn SttProvider> {
        self.providers
            .get(&self.active)
            .expect("active provider must exist in registry")
    }

    /// Name of the currently active provider.
    pub fn active_name(&self) -> &str {
        &self.active
    }

    /// Switch the active provider. Returns an error if the name is unknown.
    pub fn switch(&mut self, name: &str) -> Result<(), SttError> {
        if self.providers.contains_key(name) {
            self.active = name.to_string();
            Ok(())
        } else {
            Err(SttError::ConfigError(format!(
                "unknown provider \"{name}\"; registered: {:?}",
                self.providers.keys().collect::<Vec<_>>()
            )))
        }
    }

    /// List all registered providers and their capabilities.
    pub fn list(&self) -> Vec<(&str, HashSet<SttCapability>)> {
        self.providers
            .iter()
            .map(|(name, p)| (name.as_str(), p.capabilities()))
            .collect()
    }
}

#[cfg(test)]
#[path = "tests/provider.rs"]
mod tests;
