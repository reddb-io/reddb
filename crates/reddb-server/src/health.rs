//! Health and diagnostics types for RedDB services.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone)]
pub struct HealthIssue {
    pub component: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct HealthReport {
    pub state: HealthState,
    pub issues: Vec<HealthIssue>,
    pub diagnostics: BTreeMap<String, String>,
    pub checked_at_unix_ms: u128,
}

impl HealthReport {
    pub fn new(state: HealthState) -> Self {
        Self {
            state,
            issues: Vec::new(),
            diagnostics: BTreeMap::new(),
            checked_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        }
    }

    pub fn healthy() -> Self {
        Self::new(HealthState::Healthy)
    }

    pub fn degraded(message: impl Into<String>) -> Self {
        let mut report = Self::new(HealthState::Degraded);
        report.issues.push(HealthIssue {
            component: "engine".into(),
            message: message.into(),
        });
        report
    }

    pub fn unhealthy(message: impl Into<String>) -> Self {
        let mut report = Self::new(HealthState::Unhealthy);
        report.issues.push(HealthIssue {
            component: "engine".into(),
            message: message.into(),
        });
        report
    }

    pub fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.diagnostics.insert(key.into(), value.into());
        self
    }

    pub fn is_healthy(&self) -> bool {
        matches!(self.state, HealthState::Healthy)
    }

    pub fn issue(&mut self, component: impl Into<String>, message: impl Into<String>) {
        self.issues.push(HealthIssue {
            component: component.into(),
            message: message.into(),
        });
        self.state = HealthState::Degraded;
    }
}

pub fn storage_file_health(path: &Path) -> HealthReport {
    if !path.exists() {
        return HealthReport::degraded("database file does not exist");
    }
    let mut report = HealthReport::healthy();
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) => {
            return HealthReport::unhealthy(format!("unable to stat database file: {err}"));
        }
    };

    report = report.with_diagnostic("path", path.display().to_string());
    report = report.with_diagnostic("size_bytes", meta.len().to_string());
    report
}

pub trait HealthProvider {
    fn health(&self) -> HealthReport;
}
