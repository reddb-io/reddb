use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    pub fn as_upper(&self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
            Self::Critical => "CRITICAL",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "info" | "informational" | "none" => Some(Self::Info),
            "low" => Some(Self::Low),
            "medium" | "moderate" => Some(Self::Medium),
            "high" => Some(Self::High),
            "critical" | "crit" => Some(Self::Critical),
            _ => None,
        }
    }

    pub fn cvss_range(&self) -> (f32, f32) {
        match self {
            Self::Info => (0.0, 0.0),
            Self::Low => (0.1, 3.9),
            Self::Medium => (4.0, 6.9),
            Self::High => (7.0, 8.9),
            Self::Critical => (9.0, 10.0),
        }
    }

    pub fn from_cvss(score: f32) -> Self {
        if score >= 9.0 {
            Self::Critical
        } else if score >= 7.0 {
            Self::High
        } else if score >= 4.0 {
            Self::Medium
        } else if score > 0.0 {
            Self::Low
        } else {
            Self::Info
        }
    }

    pub fn weight(&self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }

    pub fn color_code(&self) -> &'static str {
        match self {
            Self::Info => "\x1b[37m",
            Self::Low => "\x1b[36m",
            Self::Medium => "\x1b[33m",
            Self::High => "\x1b[31m",
            Self::Critical => "\x1b[91m",
        }
    }
}

impl Default for Severity {
    fn default() -> Self {
        Self::Info
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_upper())
    }
}
