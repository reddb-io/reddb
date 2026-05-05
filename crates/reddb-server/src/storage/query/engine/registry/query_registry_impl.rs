use super::*;

impl QueryEngineRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
            default_engine: None,
        }
    }

    /// Create registry with default in-memory engine
    pub fn with_default() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(InMemoryEngineFactory));
        registry.set_default("memory");
        registry
    }

    /// Register a factory
    pub fn register(&mut self, factory: Box<dyn QueryEngineFactory>) {
        let name = factory.name().to_string();
        self.factories.insert(name, factory);
    }

    /// Set default engine
    pub fn set_default(&mut self, name: &str) {
        self.default_engine = Some(name.to_string());
    }

    /// Get engine by name
    pub fn get(&self, name: &str) -> Option<Box<dyn QueryEngine>> {
        self.factories.get(name).map(|f| f.create())
    }

    /// Get default engine
    pub fn get_default(&self) -> Option<Box<dyn QueryEngine>> {
        self.default_engine.as_ref().and_then(|name| self.get(name))
    }

    /// List registered engines
    pub fn list(&self) -> Vec<&str> {
        self.factories.keys().map(|s| s.as_str()).collect()
    }
}
