//! Ranking capability catalog persisted in `red_config`.
//!
//! Issue #918 / ADR 0035 — leaderboard rank as a *table capability*. A
//! `CREATE RANKING` statement declares order-statistics over an ordinary
//! table's score column and stores a WAL-backed catalog record (exactly
//! like the metric-descriptor catalog), reusing MVCC, the sorted index,
//! policy, and WAL. No new `sortedset` Collection model is introduced.
//!
//! This module owns three things:
//!   1. the [`RankingDescriptor`] record and its persistence in
//!      `red_config` (mirroring [`super::metric_descriptor_catalog`]);
//!   2. the narrow statement parsers (`CREATE RANKING`, `SHOW RANKINGS`,
//!      `RANK OF … IN …`) that keep the surface off the recursive-descent
//!      grammar — the same intercept pattern `READ METRIC` uses; and
//!   3. validation of the declared identifiers.
//!
//! The exact rank *computation* lives in the runtime (`impl_core`) because
//! it walks the sorted index through the existing read pipeline.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};
use crate::utils::json::{parse_json, JsonValue};

use std::time::{SystemTime, UNIX_EPOCH};

const REGISTRY_KEY: &str = "red.ranking.entries_json";

/// Default size of the exact top-K head when `TOP <k>` is omitted.
///
/// K is a tuning knob, not a wire contract (ADR 0035): it bounds how far
/// the exact, MVCC-correct walk descends before a row is considered part
/// of the approximate tail (a separate slice). 1000 comfortably covers a
/// leaderboard's visible head.
pub const DEFAULT_TOP_K: u64 = 1000;

/// A declared Ranking capability over `(table, column)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankingDescriptor {
    /// Capability name — the identity a `RANK OF … IN <name>` read binds to.
    pub name: String,
    /// The ordinary table the capability is declared on.
    pub table: String,
    /// The score column ranked over.
    pub column: String,
    /// `true` → higher score ranks first (`ORDER BY column DESC`), the
    /// leaderboard default; `false` → lower score ranks first.
    pub descending: bool,
    /// Size of the exact head (see [`DEFAULT_TOP_K`]).
    pub top_k: u64,
    pub created_at_ms: u128,
}

/// Parsed `CREATE RANKING` request, before validation/persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRankingRequest {
    pub name: String,
    pub table: String,
    pub column: String,
    pub descending: bool,
    pub top_k: u64,
}

/// Parsed `RANK OF <id> IN <name>` read request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankOfRequest {
    pub entity_id: u64,
    pub ranking: String,
}

/// Parsed `RANK RANGE <lo> TO <hi> IN <name>` read request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankRangeRequest {
    pub lo: u64,
    pub hi: u64,
    pub ranking: String,
}

/// Parsed `APPROX RANK OF <id> IN <name>` read request — the approximate
/// *tail* read (issue #923). Distinct statement head from the exact
/// `RANK OF`, so the exact head surface (#918) is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproxRankOfRequest {
    pub entity_id: u64,
    pub ranking: String,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ───────────────────────── catalog operations ─────────────────────────

pub fn create(store: &UnifiedStore, req: &CreateRankingRequest) -> RedDBResult<RankingDescriptor> {
    validate_identifier(&req.name, "ranking name")?;
    validate_identifier(&req.table, "ranking table")?;
    validate_identifier(&req.column, "ranking column")?;
    if req.top_k == 0 {
        return Err(RedDBError::Query(
            "ranking TOP must be greater than zero".to_string(),
        ));
    }

    let mut entries = load(store);
    if entries.iter().any(|entry| entry.name == req.name) {
        return Err(RedDBError::Query(format!(
            "ranking '{}' already exists",
            req.name
        )));
    }

    let descriptor = RankingDescriptor {
        name: req.name.clone(),
        table: req.table.clone(),
        column: req.column.clone(),
        descending: req.descending,
        top_k: req.top_k,
        created_at_ms: now_ms(),
    };
    entries.push(descriptor.clone());
    save(store, &entries);
    Ok(descriptor)
}

pub fn list(store: &UnifiedStore) -> Vec<RankingDescriptor> {
    load(store)
}

pub fn get(store: &UnifiedStore, name: &str) -> Option<RankingDescriptor> {
    load(store).into_iter().find(|entry| entry.name == name)
}

fn validate_identifier(value: &str, label: &str) -> RedDBResult<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(RedDBError::Query(format!(
            "invalid {label} '{value}': expected an alphanumeric/underscore identifier"
        )));
    }
    Ok(())
}

// ───────────────────────── statement parsers ─────────────────────────

/// Split SQL into whitespace tokens with `(` and `)` as standalone tokens.
fn tokenize(sql: &str) -> Vec<String> {
    let trimmed = sql.trim().trim_end_matches(';');
    let spaced = trimmed.replace('(', " ( ").replace(')', " ) ");
    spaced.split_whitespace().map(str::to_string).collect()
}

fn eq(token: &str, keyword: &str) -> bool {
    token.eq_ignore_ascii_case(keyword)
}

/// Parse `CREATE RANKING <name> ON <table> (<column> [ASC|DESC]) [TOP <k>]`.
///
/// Returns `None` for any statement that is not `CREATE RANKING …`, leaving
/// the regular SQL pipeline untouched. A malformed `CREATE RANKING` returns
/// `Some(Err(..))` so the caller surfaces a targeted error instead of the
/// generic parser's "unknown statement".
pub fn parse_create_ranking(sql: &str) -> Option<RedDBResult<CreateRankingRequest>> {
    let tokens = tokenize(sql);
    if tokens.len() < 2 || !eq(&tokens[0], "CREATE") || !eq(&tokens[1], "RANKING") {
        return None;
    }
    Some(parse_create_ranking_inner(&tokens))
}

fn parse_create_ranking_inner(tokens: &[String]) -> RedDBResult<CreateRankingRequest> {
    let err = |msg: &str| {
        RedDBError::Query(format!(
            "invalid CREATE RANKING: {msg}; expected \
             CREATE RANKING <name> ON <table> (<column> [ASC|DESC]) [TOP <k>]"
        ))
    };

    let name = tokens.get(2).ok_or_else(|| err("missing ranking name"))?;
    if !tokens.get(3).is_some_and(|t| eq(t, "ON")) {
        return Err(err("expected ON after ranking name"));
    }
    let table = tokens.get(4).ok_or_else(|| err("missing table name"))?;
    if tokens.get(5).is_none_or(|t| t != "(") {
        return Err(err("expected '(' before score column"));
    }
    let column = tokens.get(6).ok_or_else(|| err("missing score column"))?;

    // After the column: optional ASC/DESC, then mandatory ')'.
    let mut idx = 7;
    let mut descending = true; // leaderboard default: higher score first
    if let Some(dir) = tokens.get(idx) {
        if eq(dir, "ASC") {
            descending = false;
            idx += 1;
        } else if eq(dir, "DESC") {
            descending = true;
            idx += 1;
        }
    }
    if tokens.get(idx).is_none_or(|t| t != ")") {
        return Err(err("expected ')' after score column"));
    }
    idx += 1;

    // Optional TOP <k>.
    let mut top_k = DEFAULT_TOP_K;
    if let Some(top) = tokens.get(idx) {
        if !eq(top, "TOP") {
            return Err(err("unexpected token after ')'"));
        }
        let k_str = tokens
            .get(idx + 1)
            .ok_or_else(|| err("missing TOP value"))?;
        top_k = k_str
            .parse::<u64>()
            .map_err(|_| err("TOP value must be a positive integer"))?;
        if tokens.get(idx + 2).is_some() {
            return Err(err("trailing tokens after TOP value"));
        }
    }

    Ok(CreateRankingRequest {
        name: name.clone(),
        table: table.clone(),
        column: column.clone(),
        descending,
        top_k,
    })
}

/// Recognise `SHOW RANKINGS`.
pub fn parse_show_rankings(sql: &str) -> bool {
    let tokens = tokenize(sql);
    tokens.len() == 2 && eq(&tokens[0], "SHOW") && eq(&tokens[1], "RANKINGS")
}

/// Parse `RANK OF <id> IN <name>`.
///
/// Returns `None` unless the statement starts with `RANK OF`. A malformed
/// tail returns `Some(Err(..))` for a targeted error.
pub fn parse_rank_of(sql: &str) -> Option<RedDBResult<RankOfRequest>> {
    let tokens = tokenize(sql);
    if tokens.len() < 2 || !eq(&tokens[0], "RANK") || !eq(&tokens[1], "OF") {
        return None;
    }
    let err = || {
        RedDBError::Query("invalid RANK OF: expected RANK OF <id> IN <ranking-name>".to_string())
    };
    let result = (|| {
        let id_str = tokens.get(2).ok_or_else(err)?;
        let entity_id = id_str.parse::<u64>().map_err(|_| err())?;
        if !tokens.get(3).is_some_and(|t| eq(t, "IN")) {
            return Err(err());
        }
        let ranking = tokens.get(4).ok_or_else(err)?;
        if tokens.get(5).is_some() {
            return Err(err());
        }
        Ok(RankOfRequest {
            entity_id,
            ranking: ranking.clone(),
        })
    })();
    Some(result)
}

/// Parse `RANK RANGE <lo> TO <hi> IN <name>`.
///
/// Returns `None` unless the statement starts with `RANK RANGE`. A malformed
/// tail returns `Some(Err(..))` for a targeted error.
pub fn parse_rank_range(sql: &str) -> Option<RedDBResult<RankRangeRequest>> {
    let tokens = tokenize(sql);
    if tokens.len() < 2 || !eq(&tokens[0], "RANK") || !eq(&tokens[1], "RANGE") {
        return None;
    }
    let err = || {
        RedDBError::Query(
            "invalid RANK RANGE: expected RANK RANGE <lo> TO <hi> IN <ranking-name>".to_string(),
        )
    };
    let result = (|| {
        let lo_str = tokens.get(2).ok_or_else(err)?;
        let lo = lo_str.parse::<u64>().map_err(|_| err())?;
        if lo == 0 {
            return Err(err());
        }
        if !tokens.get(3).is_some_and(|t| eq(t, "TO")) {
            return Err(err());
        }
        let hi_str = tokens.get(4).ok_or_else(err)?;
        let hi = hi_str.parse::<u64>().map_err(|_| err())?;
        if hi < lo {
            return Err(err());
        }
        if !tokens.get(5).is_some_and(|t| eq(t, "IN")) {
            return Err(err());
        }
        let ranking = tokens.get(6).ok_or_else(err)?;
        if tokens.get(7).is_some() {
            return Err(err());
        }
        Ok(RankRangeRequest {
            lo,
            hi,
            ranking: ranking.clone(),
        })
    })();
    Some(result)
}

/// Parse `APPROX RANK OF <id> IN <name>` (also `APPROXIMATE RANK OF …`).
///
/// Returns `None` unless the statement starts with `APPROX[IMATE] RANK OF`,
/// so it never shadows the exact `RANK OF` head. A malformed tail returns
/// `Some(Err(..))` for a targeted error.
pub fn parse_approx_rank_of(sql: &str) -> Option<RedDBResult<ApproxRankOfRequest>> {
    let tokens = tokenize(sql);
    let approx = tokens
        .first()
        .is_some_and(|t| eq(t, "APPROX") || eq(t, "APPROXIMATE"));
    if !approx
        || !tokens.get(1).is_some_and(|t| eq(t, "RANK"))
        || !tokens.get(2).is_some_and(|t| eq(t, "OF"))
    {
        return None;
    }
    let err = || {
        RedDBError::Query(
            "invalid APPROX RANK OF: expected APPROX RANK OF <id> IN <ranking-name>".to_string(),
        )
    };
    let result = (|| {
        let id_str = tokens.get(3).ok_or_else(err)?;
        let entity_id = id_str.parse::<u64>().map_err(|_| err())?;
        if !tokens.get(4).is_some_and(|t| eq(t, "IN")) {
            return Err(err());
        }
        let ranking = tokens.get(5).ok_or_else(err)?;
        if tokens.get(6).is_some() {
            return Err(err());
        }
        Ok(ApproxRankOfRequest {
            entity_id,
            ranking: ranking.clone(),
        })
    })();
    Some(result)
}

// ───────────────────────── persistence ─────────────────────────

/// Read the latest persisted `value` Text for an arbitrary `red_config`
/// key (most-recent entity wins, mirroring the WAL-as-log-of-writes shape).
fn read_latest_config_value(store: &UnifiedStore, key: &str) -> Option<String> {
    let manager = store.get_collection("red_config")?;
    let mut all = manager.query_all(|_| true);
    all.sort_by_key(|entity| std::cmp::Reverse(entity.id.raw()));
    for entity in all {
        let EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else { continue };
        let matches = matches!(
            named.get("key"),
            Some(Value::Text(value)) if value.as_ref() == key
        );
        if matches {
            if let Some(Value::Text(value)) = named.get("value") {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn read_latest_registry_json(store: &UnifiedStore) -> Option<String> {
    read_latest_config_value(store, REGISTRY_KEY)
}

/// `red_config` key for the approximate score sketch of `(table, column)`.
/// The sketch is keyed per `(collection, score column)` (criterion 4), not
/// per ranking name, so two rankings over the same column share one sketch.
fn sketch_key(table: &str, column: &str) -> String {
    format!("red.ranking.sketch.{table}.{column}")
}

/// Persist the approximate score sketch for `(table, column)`.
pub fn save_sketch(
    store: &UnifiedStore,
    table: &str,
    column: &str,
    sketch: &super::score_sketch::ScoreSketch,
) {
    store.set_config_tree(
        &sketch_key(table, column),
        &crate::serde_json::Value::String(sketch.to_json().to_string()),
    );
}

/// Load the persisted approximate score sketch for `(table, column)`, if any.
pub fn load_sketch(
    store: &UnifiedStore,
    table: &str,
    column: &str,
) -> Option<super::score_sketch::ScoreSketch> {
    let raw = read_latest_config_value(store, &sketch_key(table, column))?;
    super::score_sketch::ScoreSketch::from_json_str(&raw)
}

fn load(store: &UnifiedStore) -> Vec<RankingDescriptor> {
    let raw = match read_latest_registry_json(store) {
        Some(raw) => raw,
        None => return Vec::new(),
    };
    let Ok(parsed) = parse_json(&raw) else {
        return Vec::new();
    };
    let Some(arr) = parsed.as_array() else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let lookup = |k: &str| obj.iter().find(|(key, _)| key == k).map(|(_, value)| value);
        let Some(name) = lookup("name").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(table) = lookup("table").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(column) = lookup("column").and_then(JsonValue::as_str) else {
            continue;
        };
        let descending = lookup("descending")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true);
        let top_k = lookup("top_k")
            .and_then(JsonValue::as_f64)
            .map(|n| n as u64)
            .filter(|k| *k > 0)
            .unwrap_or(DEFAULT_TOP_K);
        let created_at_ms = lookup("created_at_ms")
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.0);
        out.push(RankingDescriptor {
            name: name.to_string(),
            table: table.to_string(),
            column: column.to_string(),
            descending,
            top_k,
            created_at_ms: created_at_ms as u128,
        });
    }
    out
}

fn save(store: &UnifiedStore, entries: &[RankingDescriptor]) {
    let arr = crate::serde_json::Value::Array(entries.iter().map(entry_to_json).collect());
    store.set_config_tree(
        REGISTRY_KEY,
        &crate::serde_json::Value::String(arr.to_string()),
    );
}

fn entry_to_json(entry: &RankingDescriptor) -> crate::serde_json::Value {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "name".to_string(),
        crate::serde_json::Value::String(entry.name.clone()),
    );
    obj.insert(
        "table".to_string(),
        crate::serde_json::Value::String(entry.table.clone()),
    );
    obj.insert(
        "column".to_string(),
        crate::serde_json::Value::String(entry.column.clone()),
    );
    obj.insert(
        "descending".to_string(),
        crate::serde_json::Value::Bool(entry.descending),
    );
    obj.insert(
        "top_k".to_string(),
        crate::serde_json::Value::Number(entry.top_k as f64),
    );
    obj.insert(
        "created_at_ms".to_string(),
        crate::serde_json::Value::Number(entry.created_at_ms as f64),
    );
    crate::serde_json::Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_create_ranking_full() {
        let req = parse_create_ranking("CREATE RANKING top_players ON players (score DESC) TOP 50")
            .expect("recognised")
            .expect("valid");
        assert_eq!(
            req,
            CreateRankingRequest {
                name: "top_players".to_string(),
                table: "players".to_string(),
                column: "score".to_string(),
                descending: true,
                top_k: 50,
            }
        );
    }

    #[test]
    fn parse_create_ranking_defaults_desc_and_top_k() {
        let req = parse_create_ranking("create ranking r ON t (pts)")
            .expect("recognised")
            .expect("valid");
        assert!(req.descending, "direction defaults to DESC (leaderboard)");
        assert_eq!(req.top_k, DEFAULT_TOP_K);
    }

    #[test]
    fn parse_create_ranking_asc() {
        let req = parse_create_ranking("CREATE RANKING r ON t (latency ASC)")
            .expect("recognised")
            .expect("valid");
        assert!(!req.descending);
    }

    #[test]
    fn parse_create_ranking_rejects_missing_on() {
        let err = parse_create_ranking("CREATE RANKING r t (score)")
            .expect("recognised")
            .expect_err("malformed");
        assert!(err.to_string().contains("ON"), "{err}");
    }

    #[test]
    fn parse_create_ranking_ignores_other_statements() {
        assert!(parse_create_ranking("SELECT * FROM players").is_none());
        assert!(parse_create_ranking("CREATE TABLE players (id INT)").is_none());
    }

    #[test]
    fn parse_rank_of_full() {
        let req = parse_rank_of("RANK OF 42 IN top_players")
            .expect("recognised")
            .expect("valid");
        assert_eq!(
            req,
            RankOfRequest {
                entity_id: 42,
                ranking: "top_players".to_string(),
            }
        );
    }

    #[test]
    fn parse_rank_of_rejects_non_numeric_id() {
        let err = parse_rank_of("RANK OF abc IN r")
            .expect("recognised")
            .expect_err("bad id");
        assert!(err.to_string().contains("RANK OF"), "{err}");
    }

    #[test]
    fn parse_rank_of_ignores_other_statements() {
        assert!(parse_rank_of("SELECT 1").is_none());
        // `RANK() OVER (...)` is a window projection, not the RANK OF read.
        assert!(parse_rank_of("SELECT RANK() OVER (ORDER BY s DESC) FROM t").is_none());
    }

    #[test]
    fn parse_rank_range_full() {
        let req = parse_rank_range("RANK RANGE 100 TO 110 IN top_players")
            .expect("recognised")
            .expect("valid");
        assert_eq!(
            req,
            RankRangeRequest {
                lo: 100,
                hi: 110,
                ranking: "top_players".to_string(),
            }
        );
    }

    #[test]
    fn parse_rank_range_is_case_insensitive() {
        let req = parse_rank_range("rank range 1 to 5 in r")
            .expect("recognised")
            .expect("valid");
        assert_eq!(req.lo, 1);
        assert_eq!(req.hi, 5);
        assert_eq!(req.ranking, "r");
    }

    #[test]
    fn parse_rank_range_rejects_bad_bounds() {
        let err = parse_rank_range("RANK RANGE 0 TO 5 IN r")
            .expect("recognised")
            .expect_err("zero lower bound");
        assert!(err.to_string().contains("RANK RANGE"), "{err}");

        let err = parse_rank_range("RANK RANGE 5 TO 4 IN r")
            .expect("recognised")
            .expect_err("inverted bounds");
        assert!(err.to_string().contains("RANK RANGE"), "{err}");
    }

    #[test]
    fn parse_rank_range_does_not_shadow_exact_rank_of() {
        assert!(parse_rank_range("RANK OF 1 IN r").is_none());
        assert!(parse_rank_of("RANK RANGE 1 TO 2 IN r").is_none());
    }

    #[test]
    fn parse_approx_rank_of_full() {
        let req = parse_approx_rank_of("APPROX RANK OF 7 IN top_players")
            .expect("recognised")
            .expect("valid");
        assert_eq!(
            req,
            ApproxRankOfRequest {
                entity_id: 7,
                ranking: "top_players".to_string(),
            }
        );
    }

    #[test]
    fn parse_approx_rank_of_accepts_long_keyword_and_is_case_insensitive() {
        let req = parse_approx_rank_of("approximate rank of 9 in r")
            .expect("recognised")
            .expect("valid");
        assert_eq!(req.entity_id, 9);
        assert_eq!(req.ranking, "r");
    }

    #[test]
    fn parse_approx_rank_of_does_not_shadow_exact_rank_of() {
        // The exact `RANK OF` head must not be mistaken for the approx tail.
        assert!(parse_approx_rank_of("RANK OF 1 IN r").is_none());
        // And the exact parser must ignore the approx head.
        assert!(parse_rank_of("APPROX RANK OF 1 IN r").is_none());
    }

    #[test]
    fn parse_approx_rank_of_rejects_non_numeric_id() {
        let err = parse_approx_rank_of("APPROX RANK OF xyz IN r")
            .expect("recognised")
            .expect_err("bad id");
        assert!(err.to_string().contains("APPROX RANK OF"), "{err}");
    }

    #[test]
    fn parse_show_rankings_recognised() {
        assert!(parse_show_rankings("SHOW RANKINGS"));
        assert!(parse_show_rankings("show rankings;"));
        assert!(!parse_show_rankings("SHOW TABLES"));
    }
}
