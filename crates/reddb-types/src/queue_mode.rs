//! Queue-mode AST leaf (ADR 0053, RQL Phase 2 S4b).
//!
//! [`QueueMode`] is referenced by the canonical SQL AST (`CreateQueueQuery.mode`
//! and `AlterQueueQuery.mode`). The server's `storage::queue` module keeps a
//! re-export shim so existing call-sites stay untouched.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueueMode {
    Fanout,
    #[default]
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
            "WORK" | "STANDARD" | "FIFO" => Some(Self::Work),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_modes_parse_aliases_and_render_canonical_names() {
        assert_eq!(QueueMode::default(), QueueMode::Work);
        assert_eq!(QueueMode::Fanout.as_str(), "fanout");
        assert_eq!(QueueMode::Work.as_str(), "work");

        assert_eq!(QueueMode::parse("fanout"), Some(QueueMode::Fanout));
        for alias in ["work", "STANDARD", "fifo"] {
            assert_eq!(QueueMode::parse(alias), Some(QueueMode::Work), "{alias}");
        }
        assert_eq!(QueueMode::parse("broadcast"), None);
    }
}
