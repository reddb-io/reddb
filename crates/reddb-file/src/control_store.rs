use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

use crate::embedded::{RdbFileError, RdbFileResult};

pub const DEFAULT_FILE_TERM: u64 = 1;
pub const LAST_VOTE_TEMP_EXTENSION: &str = "lastvote.tmp";
pub const TERM_TEMP_EXTENSION: &str = "term.tmp";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DurableLastVote {
    pub term: u64,
    pub voted_for: Option<String>,
}

impl DurableLastVote {
    pub fn new(term: u64, voted_for: Option<String>) -> Self {
        Self { term, voted_for }
    }

    pub fn encode(&self) -> RdbFileResult<Vec<u8>> {
        let mut obj = serde_json::Map::new();
        obj.insert("term".to_string(), JsonValue::Number(self.term.into()));
        obj.insert(
            "voted_for".to_string(),
            match &self.voted_for {
                Some(id) => JsonValue::String(id.clone()),
                None => JsonValue::Null,
            },
        );
        serde_json::to_vec(&JsonValue::Object(obj))
            .map_err(|err| RdbFileError::InvalidOperation(format!("serialize last-vote: {err}")))
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        let value: JsonValue = serde_json::from_slice(bytes)
            .map_err(|err| RdbFileError::InvalidOperation(format!("parse last-vote: {err}")))?;
        let obj = value.as_object().ok_or_else(|| {
            RdbFileError::InvalidOperation("last-vote json is not an object".into())
        })?;
        let term = obj
            .get("term")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| RdbFileError::InvalidOperation("missing term".into()))?;
        let voted_for = match obj.get("voted_for") {
            None | Some(JsonValue::Null) => None,
            Some(JsonValue::String(id)) => Some(id.clone()),
            Some(_) => {
                return Err(RdbFileError::InvalidOperation(
                    "voted_for must be a string or null".into(),
                ))
            }
        };
        Ok(Self { term, voted_for })
    }
}

pub struct FileLastVoteStore {
    path: PathBuf,
}

impl FileLastVoteStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load_file(&self) -> RdbFileResult<DurableLastVote> {
        match fs::read(&self.path) {
            Ok(bytes) => DurableLastVote::decode(&bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(DurableLastVote::default())
            }
            Err(err) => Err(err.into()),
        }
    }

    pub fn persist_file(&self, vote: &DurableLastVote) -> RdbFileResult<()> {
        write_bytes_atomically(&self.path, LAST_VOTE_TEMP_EXTENSION, &vote.encode()?)
    }
}

pub struct FileTermStore {
    path: PathBuf,
    default_term: u64,
}

impl FileTermStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            default_term: DEFAULT_FILE_TERM,
        }
    }

    pub fn with_default_term(path: impl Into<PathBuf>, default_term: u64) -> Self {
        Self {
            path: path.into(),
            default_term,
        }
    }

    pub fn load_file(&self) -> RdbFileResult<u64> {
        match fs::read(&self.path) {
            Ok(bytes) => decode_term(&bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(self.default_term),
            Err(err) => Err(err.into()),
        }
    }

    pub fn persist_file(&self, term: u64) -> RdbFileResult<()> {
        write_bytes_atomically(&self.path, TERM_TEMP_EXTENSION, &encode_term(term)?)
    }
}

fn encode_term(term: u64) -> RdbFileResult<Vec<u8>> {
    let mut obj = serde_json::Map::new();
    obj.insert("term".to_string(), JsonValue::Number(term.into()));
    serde_json::to_vec(&JsonValue::Object(obj))
        .map_err(|err| RdbFileError::InvalidOperation(format!("serialize term: {err}")))
}

fn decode_term(bytes: &[u8]) -> RdbFileResult<u64> {
    let value: JsonValue = serde_json::from_slice(bytes)
        .map_err(|err| RdbFileError::InvalidOperation(format!("parse term: {err}")))?;
    value
        .get("term")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| RdbFileError::InvalidOperation("missing term".into()))
}

fn write_bytes_atomically(path: &Path, temp_extension: &str, bytes: &[u8]) -> RdbFileResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(temp_extension);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("reddb-file-control-{name}-{suffix}.json"))
    }

    #[test]
    fn last_vote_round_trips_and_defaults() {
        let path = temp_path("lastvote");
        let store = FileLastVoteStore::new(&path);
        assert_eq!(
            store.load_file().expect("default"),
            DurableLastVote::default()
        );

        let vote = DurableLastVote::new(9, Some("replica-a".into()));
        store.persist_file(&vote).expect("persist");
        assert_eq!(
            FileLastVoteStore::new(&path).load_file().expect("load"),
            vote
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn term_round_trips_and_defaults() {
        let path = temp_path("term");
        let store = FileTermStore::with_default_term(&path, 3);
        assert_eq!(store.load_file().expect("default"), 3);

        store.persist_file(12).expect("persist");
        assert_eq!(FileTermStore::new(&path).load_file().expect("load"), 12);

        let _ = fs::remove_file(path);
    }
}
