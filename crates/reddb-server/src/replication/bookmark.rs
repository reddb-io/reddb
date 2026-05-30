//! Causal bookmark token helpers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CausalBookmark {
    term: u64,
    commit_lsn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkDecodeError {
    InvalidPrefix,
    InvalidLength,
    InvalidHex,
}

impl std::fmt::Display for BookmarkDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPrefix => write!(f, "invalid causal bookmark prefix"),
            Self::InvalidLength => write!(f, "invalid causal bookmark length"),
            Self::InvalidHex => write!(f, "invalid causal bookmark hex payload"),
        }
    }
}

impl std::error::Error for BookmarkDecodeError {}

impl CausalBookmark {
    pub fn new(term: u64, commit_lsn: u64) -> Self {
        Self { term, commit_lsn }
    }

    pub fn term(self) -> u64 {
        self.term
    }

    pub fn commit_lsn(self) -> u64 {
        self.commit_lsn
    }

    pub fn encode(self) -> String {
        format!("rbm1.{:016x}{:016x}", self.term, self.commit_lsn)
    }

    pub fn decode(token: &str) -> Result<Self, BookmarkDecodeError> {
        let Some(payload) = token.strip_prefix("rbm1.") else {
            return Err(BookmarkDecodeError::InvalidPrefix);
        };
        if payload.len() != 32 {
            return Err(BookmarkDecodeError::InvalidLength);
        }
        let term =
            u64::from_str_radix(&payload[..16], 16).map_err(|_| BookmarkDecodeError::InvalidHex)?;
        let commit_lsn =
            u64::from_str_radix(&payload[16..], 16).map_err(|_| BookmarkDecodeError::InvalidHex)?;
        Ok(Self { term, commit_lsn })
    }
}
