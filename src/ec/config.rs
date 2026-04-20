use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcReducer {
    Sum,
    Max,
    Min,
    Count,
    Average,
    Last,
}

impl EcReducer {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "max" => Self::Max,
            "min" => Self::Min,
            "count" => Self::Count,
            "average" | "avg" => Self::Average,
            "last" | "lww" => Self::Last,
            _ => Self::Sum,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Max => "max",
            Self::Min => "min",
            Self::Count => "count",
            Self::Average => "average",
            Self::Last => "last",
        }
    }

    pub fn apply(&self, current: f64, incoming: f64, count: u64) -> f64 {
        match self {
            Self::Sum => current + incoming,
            Self::Max => current.max(incoming),
            Self::Min => {
                if current == 0.0 && count == 0 {
                    incoming
                } else {
                    current.min(incoming)
                }
            }
            Self::Count => current + 1.0,
            Self::Average => {
                if count == 0 {
                    incoming
                } else {
                    (current * count as f64 + incoming) / (count as f64 + 1.0)
                }
            }
            Self::Last => incoming,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcMode {
    Sync,
    Async,
}

impl EcMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "sync" | "immediate" => Self::Sync,
            _ => Self::Async,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EcFieldConfig {
    pub collection: String,
    pub field: String,
    pub field_path: Option<String>,
    pub reducer: EcReducer,
    pub initial_value: f64,
    pub mode: EcMode,
    pub consolidation_interval_secs: u64,
    pub consolidation_window_hours: u64,
    pub retention_days: u64,
}

impl EcFieldConfig {
    pub fn new(collection: &str, field: &str) -> Self {
        Self {
            collection: collection.to_string(),
            field: field.to_string(),
            field_path: None,
            reducer: EcReducer::Sum,
            initial_value: 0.0,
            mode: EcMode::Async,
            consolidation_interval_secs: 60,
            consolidation_window_hours: 24,
            retention_days: 7,
        }
    }

    pub fn tx_collection_name(&self) -> String {
        format!("red_ec_tx_{}_{}", self.collection, self.field)
    }
}

pub struct EcRegistry {
    fields: RwLock<HashMap<(String, String), EcFieldConfig>>,
}

impl EcRegistry {
    pub fn new() -> Self {
        Self {
            fields: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, config: EcFieldConfig) {
        let key = (config.collection.clone(), config.field.clone());
        self.fields
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, config);
    }

    pub fn get(&self, collection: &str, field: &str) -> Option<EcFieldConfig> {
        self.fields
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(collection.to_string(), field.to_string()))
            .cloned()
    }

    pub fn is_ec_field(&self, collection: &str, field: &str) -> bool {
        self.fields
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(collection.to_string(), field.to_string()))
    }

    pub fn all_configs(&self) -> Vec<EcFieldConfig> {
        self.fields
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn async_configs(&self) -> Vec<EcFieldConfig> {
        self.fields
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|c| c.mode == EcMode::Async)
            .cloned()
            .collect()
    }

    pub fn load_from_config_store(&self, store: &crate::storage::unified::store::UnifiedStore) {
        let manager = match store.get_collection("red_config") {
            Some(m) => m,
            None => return,
        };

        let mut ec_collections: HashMap<String, Vec<String>> = HashMap::new();

        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if let Some(k) = key {
                    if let Some(rest) = k.strip_prefix("red.config.ec.") {
                        if let Some(val) = row.get_field("value") {
                            if rest.ends_with(".fields") {
                                let collection = rest.trim_end_matches(".fields");
                                if let crate::storage::schema::Value::Text(fields_str) = val {
                                    let fields: Vec<String> = fields_str
                                        .trim_matches(|c| c == '[' || c == ']')
                                        .split(',')
                                        .map(|s| {
                                            s.trim()
                                                .trim_matches('"')
                                                .trim_matches('\'')
                                                .to_string()
                                        })
                                        .filter(|s| !s.is_empty())
                                        .collect();
                                    ec_collections.insert(collection.to_string(), fields);
                                }
                            }
                        }
                    }
                }
            }
            true
        });

        for (collection, fields) in ec_collections {
            for field in fields {
                let mut config = EcFieldConfig::new(&collection, &field);

                // Load per-field overrides from red_config
                let prefix = format!("red.config.ec.{}.{}", collection, field);
                manager.for_each_entity(|entity| {
                    if let Some(row) = entity.data.as_row() {
                        let key = row.get_field("key").and_then(|v| match v {
                            crate::storage::schema::Value::Text(s) => Some(s.clone()),
                            _ => None,
                        });
                        let val = row.get_field("value");
                        if let (Some(k), Some(v)) = (key, val) {
                            if k == format!("{}.reducer", prefix) {
                                if let crate::storage::schema::Value::Text(s) = v {
                                    config.reducer = EcReducer::from_str(s);
                                }
                            } else if k == format!("{}.mode", prefix) {
                                if let crate::storage::schema::Value::Text(s) = v {
                                    config.mode = EcMode::from_str(s);
                                }
                            } else if k == format!("{}.interval_secs", prefix) {
                                if let crate::storage::schema::Value::Integer(n) = v {
                                    config.consolidation_interval_secs = *n as u64;
                                }
                            }
                        }
                    }
                    true
                });

                self.register(config);
            }
        }
    }
}
