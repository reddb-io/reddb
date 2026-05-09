#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    Fanout,
    Work,
}

impl QueueMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fanout => "fanout",
            Self::Work => "work",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "FANOUT" => Some(Self::Fanout),
            "WORK" => Some(Self::Work),
            _ => None,
        }
    }
}

impl Default for QueueMode {
    fn default() -> Self {
        Self::Work
    }
}
