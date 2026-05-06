//! Adversarial-input fixtures.
//!
//! Each entry is an `(name, input)` pair. The same corpus is
//! consumed by:
//!   - the panic-safety property tests in
//!     `tests/parser_hardening.rs`
//!   - the fuzz seed corpus loaded by `fuzz_targets/sql_parser.rs`
//!
//! Adding a regression case here automatically extends both
//! safety nets.

/// Adversarial inputs that historically (or theoretically) trip
/// recursion / memory paths. None of these should panic; all
/// should either parse or return an `Err`.
pub fn adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("empty", String::new()),
        ("only_whitespace", "    \n\t  ".to_string()),
        (
            "deep_parens_50",
            format!("SELECT {}1{} FROM t", "(".repeat(50), ")".repeat(50),),
        ),
        (
            "deep_parens_500",
            format!("SELECT {}1{} FROM t", "(".repeat(500), ")".repeat(500),),
        ),
        (
            "deep_not_chain",
            format!("SELECT * FROM t WHERE {} a = 1", "NOT ".repeat(500),),
        ),
        (
            "long_identifier",
            format!("SELECT * FROM {}", "x".repeat(10_000),),
        ),
        ("oversized_input", "a".repeat(2 * 1024 * 1024)),
        ("unbalanced_parens", "SELECT (((1 FROM t".to_string()),
        ("dangling_comma", "SELECT a, b, FROM t".to_string()),
        ("missing_from", "SELECT x WHERE y = 1".to_string()),
        ("eof_mid_stmt", "SELECT * FROM".to_string()),
        ("garbage_bytes", "@#$%^&*()_+|}{:?><".to_string()),
        (
            "invalid_escape_in_string",
            r"SELECT '\\xff' FROM t".to_string(),
        ),
        ("leading_number_ident", "SELECT 1abc FROM t".to_string()),
        ("nul_byte", "SELECT * FROM t\0".to_string()),
        (
            "very_long_string_lit",
            format!("SELECT '{}' FROM t", "x".repeat(100_000),),
        ),
    ]
}

/// Adversarial inputs that target the time-series DSL surface (issue
/// #102). These exercise the `parse_create_timeseries_body`,
/// `parse_create_hypertable_body`, the bare `CHUNK_INTERVAL` literal
/// validator, and the materialized-view envelope continuous
/// aggregates ride through today.
///
/// Every entry must surface as `Ok` *or* a structured `Err` — never a
/// panic. The fuzz seed corpus in `fuzz/corpus/sql_parser/` is
/// derived from this same list.
pub fn timeseries_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // CREATE TIMESERIES surface --------------------------------
        (
            "ts_eof_after_create",
            "CREATE TIMESERIES".to_string(),
        ),
        (
            "ts_eof_after_name",
            "CREATE TIMESERIES m1".to_string(),
        ),
        (
            "ts_retention_no_value",
            "CREATE TIMESERIES m1 RETENTION".to_string(),
        ),
        (
            "ts_retention_negative",
            "CREATE TIMESERIES m1 RETENTION -90 d".to_string(),
        ),
        (
            "ts_retention_unknown_unit",
            "CREATE TIMESERIES m1 RETENTION 90 fortnights".to_string(),
        ),
        (
            "ts_retention_zero",
            "CREATE TIMESERIES m1 RETENTION 0 d".to_string(),
        ),
        (
            "ts_chunk_size_negative",
            "CREATE TIMESERIES m1 CHUNK_SIZE -1".to_string(),
        ),
        (
            "ts_downsample_dangling_comma",
            "CREATE TIMESERIES m1 DOWNSAMPLE 1h:5m:avg,".to_string(),
        ),
        (
            "ts_downsample_bad_aggregation_separator",
            "CREATE TIMESERIES m1 DOWNSAMPLE 1h-5m-avg".to_string(),
        ),
        // CREATE HYPERTABLE surface --------------------------------
        (
            "ht_eof_after_create",
            "CREATE HYPERTABLE".to_string(),
        ),
        (
            "ht_missing_time_column",
            "CREATE HYPERTABLE metrics CHUNK_INTERVAL '1d'".to_string(),
        ),
        (
            "ht_missing_chunk_interval",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts".to_string(),
        ),
        (
            "ht_chunk_interval_long_form",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1 day'".to_string(),
        ),
        (
            "ht_chunk_interval_negative",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '-1d'".to_string(),
        ),
        (
            "ht_chunk_interval_unknown_unit",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1y'".to_string(),
        ),
        (
            "ht_chunk_interval_bare_int",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL 86400".to_string(),
        ),
        (
            "ht_ttl_unknown_unit",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' TTL '1 fortnight'"
                .to_string(),
        ),
        (
            "ht_oversized_body",
            format!(
                "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' {}",
                "TTL '1d' ".repeat(2_000)
            ),
        ),
        (
            "ht_deep_paren_after_name",
            format!(
                "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL {}'1d'{}",
                "(".repeat(50),
                ")".repeat(50),
            ),
        ),
        (
            "ht_nul_byte",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'\0".to_string(),
        ),
        // Continuous aggregate envelope ---------------------------
        (
            "cagg_eof_after_view",
            "CREATE MATERIALIZED VIEW".to_string(),
        ),
        (
            "cagg_missing_as",
            "CREATE MATERIALIZED VIEW mv SELECT 1 FROM t".to_string(),
        ),
        (
            "cagg_garbage_body",
            "CREATE MATERIALIZED VIEW mv AS @#$%".to_string(),
        ),
    ]
}

/// Adversarial inputs that target the migration DSL surface (issue
/// #88). These exercise the `parse_create_migration_body`,
/// `parse_apply_migration`, `parse_rollback_migration_after_keyword`,
/// and `parse_explain_migration_after_keyword` entry points.
pub fn migration_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("migration_eof_after_create", "CREATE MIGRATION".to_string()),
        (
            "migration_eof_after_name",
            "CREATE MIGRATION m1".to_string(),
        ),
        (
            "migration_eof_after_depends",
            "CREATE MIGRATION m1 DEPENDS ON".to_string(),
        ),
        (
            "migration_dangling_depends_comma",
            "CREATE MIGRATION m1 DEPENDS ON a, AS CREATE TABLE t (id INTEGER)".to_string(),
        ),
        (
            "migration_apply_eof",
            "APPLY MIGRATION".to_string(),
        ),
        (
            "migration_rollback_eof",
            "ROLLBACK MIGRATION".to_string(),
        ),
        (
            "migration_explain_eof",
            "EXPLAIN MIGRATION".to_string(),
        ),
        (
            "migration_apply_for_no_tenant",
            "APPLY MIGRATION m1 FOR".to_string(),
        ),
        (
            "migration_long_name",
            format!("CREATE MIGRATION {} AS CREATE TABLE t (id INTEGER)", "m".repeat(10_000)),
        ),
        (
            "migration_deep_paren_body",
            format!(
                "CREATE MIGRATION m1 AS SELECT {}1{} FROM t",
                "(".repeat(500),
                ")".repeat(500),
            ),
        ),
        (
            "migration_oversized_body",
            format!("CREATE MIGRATION m1 AS {}", "a".repeat(2 * 1024 * 1024)),
        ),
        (
            "migration_nul_byte",
            "CREATE MIGRATION m1 AS CREATE TABLE t (id INTEGER)\0".to_string(),
        ),
        (
            "migration_garbage",
            "CREATE MIGRATION @#$%".to_string(),
        ),
    ]
}

/// Adversarial inputs that target the geo / spatial surface (issue
/// #104). These exercise `parse_search_spatial`, RTREE index DDL,
/// and the geo scalar functions in projection position.
///
/// Out-of-range coordinates (lat=91, lon=-181) are included on
/// purpose: the parser does not semantically validate ranges today,
/// so these inputs *parse* (which is fine — the harness only
/// guarantees no panics). Once range validation is added in a future
/// slice the FIXME pin in `tests/geo_parser.rs` will start to fail
/// and force a snapshot refresh.
pub fn geo_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // ----- bare / EOF shapes ---------------------------------
        ("geo_eof_after_search_spatial", "SEARCH SPATIAL".to_string()),
        (
            "geo_eof_after_radius",
            "SEARCH SPATIAL RADIUS".to_string(),
        ),
        (
            "geo_eof_after_nearest",
            "SEARCH SPATIAL NEAREST 0.0 0.0 K".to_string(),
        ),
        (
            "geo_eof_after_bbox",
            "SEARCH SPATIAL BBOX 0.0 0.0".to_string(),
        ),
        // ----- out-of-range latitude / longitude -----------------
        (
            "geo_lat_91_out_of_range",
            "SEARCH SPATIAL RADIUS 91.0 0.0 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_lon_181_out_of_range",
            "SEARCH SPATIAL RADIUS 0.0 181.0 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_lat_neg91",
            // Note: leading unary `-` does not lex as a float here;
            // it tokenises as Minus + Float(91.0). The parse_float
            // call only accepts Float/Integer, so this *errors*
            // before even reaching the range check. See FIXME pin.
            "SEARCH SPATIAL NEAREST -91.0 0.0 K 5 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_lon_neg181",
            "SEARCH SPATIAL NEAREST 0.0 -181.0 K 5 COLLECTION c COLUMN col".to_string(),
        ),
        // ----- numeric edge cases --------------------------------
        (
            "geo_radius_negative",
            // `parse_float` rejects unary minus; the parser surfaces
            // an "expected number" error at the radius position.
            "SEARCH SPATIAL RADIUS 0.0 0.0 -10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_radius_zero",
            "SEARCH SPATIAL RADIUS 0.0 0.0 0.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_k_zero",
            "SEARCH SPATIAL NEAREST 0.0 0.0 K 0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_nearest_k_negative",
            "SEARCH SPATIAL NEAREST 0.0 0.0 K -1 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_radius_nan_literal",
            // `NaN` is not a recognised literal in the lexer; this
            // tokenises as an Ident and `parse_float` errors with
            // "expected number". The harness only asserts no panic.
            "SEARCH SPATIAL RADIUS NaN NaN 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        (
            "geo_radius_infinity_literal",
            "SEARCH SPATIAL RADIUS Infinity 0.0 10.0 COLLECTION c COLUMN col".to_string(),
        ),
        // ----- structural malformations --------------------------
        (
            "geo_radius_missing_collection_kw",
            "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 sites COLUMN col".to_string(),
        ),
        (
            "geo_radius_missing_column_kw",
            "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION sites col".to_string(),
        ),
        (
            "geo_nearest_missing_k_kw",
            "SEARCH SPATIAL NEAREST 0.0 0.0 5 COLLECTION sites COLUMN col".to_string(),
        ),
        (
            "geo_unknown_subcommand",
            "SEARCH SPATIAL POLYGON 0.0 0.0 COLLECTION c COLUMN col".to_string(),
        ),
        // ----- RTREE index DDL -----------------------------------
        (
            "geo_rtree_no_columns",
            "CREATE INDEX gix ON sites () USING RTREE".to_string(),
        ),
        (
            "geo_rtree_unknown_method",
            "CREATE INDEX gix ON sites (location) USING WRONGTREE".to_string(),
        ),
        // ----- distance fns --------------------------------------
        (
            "geo_distance_no_args",
            "SELECT GEO_DISTANCE() FROM t".to_string(),
        ),
        (
            "geo_haversine_dangling_comma",
            "SELECT HAVERSINE(0.0, 0.0, 1.0,) FROM t".to_string(),
        ),
        // ----- bulk / DoS shapes ---------------------------------
        (
            "geo_radius_oversized",
            format!(
                "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION {} COLUMN col",
                "c".repeat(10_000),
            ),
        ),
        (
            "geo_nul_byte",
            "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION c COLUMN col\0".to_string(),
        ),
        (
            "geo_garbage_after_radius",
            "SEARCH SPATIAL RADIUS @#$% COLLECTION c COLUMN col".to_string(),
        ),
    ]
}
pub fn graph_dsl_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("graph_match_eof", "MATCH".to_string()),
        ("graph_match_open_paren_eof", "MATCH (".to_string()),
        ("graph_match_open_alias_eof", "MATCH (a".to_string()),
        ("graph_match_no_return", "MATCH (a)-[r]->(b)".to_string()),
        (
            "graph_match_unbalanced_bracket",
            "MATCH (a)-[r:KNOWS->(b) RETURN a".to_string(),
        ),
        (
            "graph_match_unbalanced_brace",
            "MATCH (a {name: 'x') RETURN a".to_string(),
        ),
        (
            "graph_match_dangling_props_comma",
            "MATCH (a:person {name: 'x',}) RETURN a".to_string(),
        ),
        (
            "graph_match_dangling_return_comma",
            "MATCH (a)-[r]->(b) RETURN a,".to_string(),
        ),
        (
            "graph_match_bad_var_length",
            "MATCH (a)-[r*1..]->(b) RETURN a".to_string(),
        ),
        (
            "graph_match_var_length_no_max",
            "MATCH (a)-[r*..3]->(b) RETURN a".to_string(),
        ),
        (
            "graph_match_deep_chain",
            // 200 hops of `(x)-[]->` to stress the depth tracker.
            format!(
                "MATCH (a){} RETURN a",
                "-[]->(x)".repeat(200),
            ),
        ),
        (
            "graph_match_long_alias",
            format!("MATCH ({}) RETURN a", "a".repeat(10_000)),
        ),
        (
            "graph_match_oversized",
            format!("MATCH (a) RETURN a {}", " ".repeat(2 * 1024 * 1024)),
        ),
        (
            "graph_match_nul_byte",
            "MATCH (a)-[r]->(b) RETURN a\0".to_string(),
        ),
        ("graph_match_garbage", "MATCH @#$%".to_string()),
        // PATH FROM ... TO ... surface
        ("graph_path_eof", "PATH".to_string()),
        ("graph_path_no_to", "PATH FROM host('x')".to_string()),
        ("graph_path_garbage_via", "PATH FROM host('a') TO host('b') VIA @#$%".to_string()),
        // GRAPH command surface
        ("graph_cmd_eof", "GRAPH".to_string()),
        ("graph_cmd_unknown_subcmd", "GRAPH NONESUCH".to_string()),
        ("graph_neighborhood_no_src", "GRAPH NEIGHBORHOOD".to_string()),
        (
            "graph_shortest_path_no_to",
            "GRAPH SHORTEST_PATH 'a'".to_string(),
        ),
        (
            "graph_traverse_garbage_strategy",
            "GRAPH TRAVERSE 'a' STRATEGY".to_string(),
        ),
        // Phase A: CREATE NODE / CREATE EDGE shapes do not exist in
        // the SQL grammar (they ship as API calls). The parser must
        // surface a Syntax error rather than panicking.
        (
            "graph_create_node_attempt",
            "CREATE NODE (a:person {name: 'x'})".to_string(),
        ),
        (
            "graph_create_edge_attempt",
            "CREATE EDGE (a)-[:KNOWS]->(b)".to_string(),
        ),
    ]
}

pub fn queue_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // CREATE QUEUE — invalid MAX_SIZE values.
        ("queue_create_eof_after_keyword", "CREATE QUEUE".to_string()),
        ("queue_create_missing_name", "CREATE QUEUE MAX_SIZE 100".to_string()),
        (
            "queue_create_max_size_negative",
            "CREATE QUEUE q MAX_SIZE -1".to_string(),
        ),
        (
            "queue_create_max_size_zero",
            "CREATE QUEUE q MAX_SIZE 0".to_string(),
        ),
        (
            "queue_create_max_size_non_numeric",
            "CREATE QUEUE q MAX_SIZE forever".to_string(),
        ),
        (
            "queue_create_max_size_eof",
            "CREATE QUEUE q MAX_SIZE".to_string(),
        ),
        (
            "queue_create_max_size_overflow",
            "CREATE QUEUE q MAX_SIZE 99999999999999999999".to_string(),
        ),
        (
            "queue_create_dangling_with",
            "CREATE QUEUE q WITH".to_string(),
        ),
        (
            "queue_create_with_ttl_no_value",
            "CREATE QUEUE q WITH TTL".to_string(),
        ),
        (
            "queue_create_with_dlq_no_name",
            "CREATE QUEUE q WITH DLQ".to_string(),
        ),
        // PUSH — malformed and oversized payloads.
        ("queue_push_eof", "QUEUE PUSH".to_string()),
        ("queue_push_missing_payload", "QUEUE PUSH q".to_string()),
        (
            "queue_push_unterminated_string",
            "QUEUE PUSH q 'no closing".to_string(),
        ),
        (
            "queue_push_unbalanced_json",
            "QUEUE PUSH q {job: 'hello'".to_string(),
        ),
        (
            "queue_push_oversized_string_payload",
            format!("QUEUE PUSH q '{}'", "x".repeat(2 * 1024 * 1024)),
        ),
        (
            "queue_push_oversized_input",
            "QUEUE PUSH q ".to_string() + &"x".repeat(2 * 1024 * 1024),
        ),
        (
            "queue_push_priority_no_value",
            "QUEUE PUSH q 'x' PRIORITY".to_string(),
        ),
        // POP / aliases.
        ("queue_pop_eof", "QUEUE POP".to_string()),
        (
            "queue_pop_count_no_value",
            "QUEUE POP q COUNT".to_string(),
        ),
        // Consumer group syntax.
        ("queue_group_create_eof", "QUEUE GROUP CREATE".to_string()),
        (
            "queue_group_create_missing_group",
            "QUEUE GROUP CREATE q".to_string(),
        ),
        (
            "queue_read_missing_group_keyword",
            "QUEUE READ q workers".to_string(),
        ),
        (
            "queue_read_missing_consumer_name",
            "QUEUE READ q GROUP g CONSUMER".to_string(),
        ),
        (
            "queue_claim_missing_min_idle",
            "QUEUE CLAIM q GROUP g CONSUMER c".to_string(),
        ),
        (
            "queue_claim_min_idle_non_numeric",
            "QUEUE CLAIM q GROUP g CONSUMER c MIN_IDLE forever".to_string(),
        ),
        (
            "queue_ack_missing_message_id",
            "QUEUE ACK q GROUP g".to_string(),
        ),
        (
            "queue_unknown_subcommand",
            "QUEUE FROBNICATE q".to_string(),
        ),
        // Bytes-level adversarial inputs.
        (
            "queue_garbage_after_keyword",
            "QUEUE @#$%".to_string(),
        ),
        (
            "queue_nul_byte",
            "CREATE QUEUE q\0".to_string(),
        ),
        (
            "queue_long_name",
            format!("CREATE QUEUE {}", "q".repeat(10_000)),
        ),
    ]
}

pub fn ask_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("ask_eof_after_keyword", "ASK".to_string()),
        ("ask_eof_after_question", "ASK 'why?'".to_string()),
        ("ask_missing_question", "ASK USING openai".to_string()),
        (
            "ask_unterminated_string",
            "ASK 'open question without closing quote".to_string(),
        ),
        (
            "ask_using_no_provider",
            "ASK 'q' USING".to_string(),
        ),
        (
            "ask_model_no_string",
            "ASK 'q' MODEL".to_string(),
        ),
        (
            "ask_model_unquoted_ident",
            // MODEL slot expects a string literal, not a bare ident.
            "ASK 'q' MODEL gpt4".to_string(),
        ),
        (
            "ask_depth_no_int",
            "ASK 'q' DEPTH".to_string(),
        ),
        (
            "ask_depth_negative",
            "ASK 'q' DEPTH -1".to_string(),
        ),
        (
            "ask_limit_garbage",
            "ASK 'q' LIMIT @#$%".to_string(),
        ),
        (
            "ask_collection_no_ident",
            "ASK 'q' COLLECTION".to_string(),
        ),
        (
            "ask_more_than_five_clauses",
            // Parser caps the optional-clause loop at 5 iterations;
            // a 6th repeat must round-trip without panic (it parses
            // partially and the trailing tokens become a follow-on
            // statement which the top-level loop rejects).
            "ASK 'q' USING a MODEL 'm' DEPTH 1 LIMIT 2 COLLECTION c USING b".to_string(),
        ),
        (
            "ask_oversized_question",
            format!("ASK '{}'", "x".repeat(100_000)),
        ),
        (
            "ask_unicode_question",
            "ASK '雪花飘落 ❄ ε≈μ' USING openai".to_string(),
        ),
        (
            "ask_zero_width_unicode",
            "ASK '\u{200b}\u{feff}q' USING openai".to_string(),
        ),
        (
            "ask_nul_byte_in_question",
            "ASK 'q\0nul' USING openai".to_string(),
        ),
        (
            "ask_garbage_payload",
            "ASK @#$%".to_string(),
        ),
        // SEARCH CONTEXT adversarial shapes
        (
            "search_context_eof",
            "SEARCH CONTEXT".to_string(),
        ),
        (
            "search_context_missing_string",
            "SEARCH CONTEXT FIELD x".to_string(),
        ),
        (
            "search_context_unterminated_string",
            "SEARCH CONTEXT 'open".to_string(),
        ),
        (
            "search_context_field_no_ident",
            "SEARCH CONTEXT 'q' FIELD".to_string(),
        ),
        (
            "search_context_collection_no_ident",
            "SEARCH CONTEXT 'q' COLLECTION".to_string(),
        ),
        (
            "search_context_oversized_query",
            format!("SEARCH CONTEXT '{}'", "x".repeat(100_000)),
        ),
    ]
}

pub fn timeseries_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // CREATE TIMESERIES surface --------------------------------
        (
            "ts_eof_after_create",
            "CREATE TIMESERIES".to_string(),
        ),
        (
            "ts_eof_after_name",
            "CREATE TIMESERIES m1".to_string(),
        ),
        (
            "ts_retention_no_value",
            "CREATE TIMESERIES m1 RETENTION".to_string(),
        ),
        (
            "ts_retention_negative",
            "CREATE TIMESERIES m1 RETENTION -90 d".to_string(),
        ),
        (
            "ts_retention_unknown_unit",
            "CREATE TIMESERIES m1 RETENTION 90 fortnights".to_string(),
        ),
        (
            "ts_retention_zero",
            "CREATE TIMESERIES m1 RETENTION 0 d".to_string(),
        ),
        (
            "ts_chunk_size_negative",
            "CREATE TIMESERIES m1 CHUNK_SIZE -1".to_string(),
        ),
        (
            "ts_downsample_dangling_comma",
            "CREATE TIMESERIES m1 DOWNSAMPLE 1h:5m:avg,".to_string(),
        ),
        (
            "ts_downsample_bad_aggregation_separator",
            "CREATE TIMESERIES m1 DOWNSAMPLE 1h-5m-avg".to_string(),
        ),
        // CREATE HYPERTABLE surface --------------------------------
        (
            "ht_eof_after_create",
            "CREATE HYPERTABLE".to_string(),
        ),
        (
            "ht_missing_time_column",
            "CREATE HYPERTABLE metrics CHUNK_INTERVAL '1d'".to_string(),
        ),
        (
            "ht_missing_chunk_interval",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts".to_string(),
        ),
        (
            "ht_chunk_interval_long_form",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1 day'".to_string(),
        ),
        (
            "ht_chunk_interval_negative",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '-1d'".to_string(),
        ),
        (
            "ht_chunk_interval_unknown_unit",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1y'".to_string(),
        ),
        (
            "ht_chunk_interval_bare_int",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL 86400".to_string(),
        ),
        (
            "ht_ttl_unknown_unit",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' TTL '1 fortnight'"
                .to_string(),
        ),
        (
            "ht_oversized_body",
            format!(
                "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' {}",
                "TTL '1d' ".repeat(2_000)
            ),
        ),
        (
            "ht_deep_paren_after_name",
            format!(
                "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL {}'1d'{}",
                "(".repeat(50),
                ")".repeat(50),
            ),
        ),
        (
            "ht_nul_byte",
            "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'\0".to_string(),
        ),
        // Continuous aggregate envelope ---------------------------
        (
            "cagg_eof_after_view",
            "CREATE MATERIALIZED VIEW".to_string(),
        ),
        (
            "cagg_missing_as",
            "CREATE MATERIALIZED VIEW mv SELECT 1 FROM t".to_string(),
        ),
        (
            "cagg_garbage_body",
            "CREATE MATERIALIZED VIEW mv AS @#$%".to_string(),
        ),
    ]
}

pub fn vector_search_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // ---- malformed vector literals -------------------------
        ("vector_eof_after_lbracket", "SEARCH SIMILAR [".to_string()),
        (
            "vector_unterminated_literal",
            "SEARCH SIMILAR [0.1, 0.2 COLLECTION c".to_string(),
        ),
        (
            "vector_dangling_comma",
            "SEARCH SIMILAR [0.1, 0.2,] COLLECTION c".to_string(),
        ),
        (
            "vector_empty_literal",
            "VECTOR SEARCH e SIMILAR TO [] LIMIT 5".to_string(),
        ),
        (
            "vector_garbage_in_literal",
            "SEARCH SIMILAR [0.1, @#$, 0.3] COLLECTION c".to_string(),
        ),
        // ---- gigantic dims -------------------------------------
        (
            "vector_huge_dim_4096",
            {
                let dims: Vec<String> = (0..4096).map(|i| format!("{:.4}", i as f32 / 4096.0)).collect();
                format!("SEARCH SIMILAR [{}] COLLECTION big LIMIT 5", dims.join(", "))
            },
        ),
        (
            "vector_silly_dim_50000_under_limit",
            // Stays under the 1 MiB max_input_bytes default by
            // virtue of short floats. Targets the vector-literal
            // comma-loop without tripping the size guard.
            {
                let dims: Vec<String> = (0..50_000).map(|_| "0.1".to_string()).collect();
                format!("SEARCH SIMILAR [{}] COLLECTION big", dims.join(","))
            },
        ),
        // ---- NaN / Infinity floats -----------------------------
        // The lexer accepts these as identifiers (NaN / Infinity
        // are not numeric tokens), so the parser bails inside
        // `parse_float`. Pin the no-panic invariant.
        (
            "vector_nan_in_literal",
            "SEARCH SIMILAR [0.1, NaN, 0.3] COLLECTION c".to_string(),
        ),
        (
            "vector_inf_in_literal",
            "SEARCH SIMILAR [0.1, Infinity, 0.3] COLLECTION c".to_string(),
        ),
        (
            "vector_neg_inf_in_literal",
            "SEARCH SIMILAR [-Infinity, 0.0] COLLECTION c".to_string(),
        ),
        (
            "vector_huge_float",
            "SEARCH SIMILAR [1e308, -1e308] COLLECTION c".to_string(),
        ),
        (
            "vector_too_huge_float",
            "SEARCH SIMILAR [1e500] COLLECTION c".to_string(),
        ),
        // ---- structural breakage -------------------------------
        ("similar_eof", "SEARCH SIMILAR".to_string()),
        ("similar_no_collection", "SEARCH SIMILAR [0.1, 0.2]".to_string()),
        (
            "similar_no_collection_name",
            "SEARCH SIMILAR [0.1] COLLECTION".to_string(),
        ),
        (
            "similar_text_no_string",
            "SEARCH SIMILAR TEXT COLLECTION c".to_string(),
        ),
        (
            "similar_unterminated_text",
            "SEARCH SIMILAR TEXT 'unterminated COLLECTION c".to_string(),
        ),
        // ---- AUTO EMBED malformed shapes -----------------------
        (
            "auto_embed_eof_after_keyword",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED".to_string(),
        ),
        (
            "auto_embed_empty_field_list",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED ()".to_string(),
        ),
        (
            "auto_embed_dangling_comma",
            "INSERT INTO t (a, b) VALUES ('x', 'y') WITH AUTO EMBED (a, b,)".to_string(),
        ),
        (
            "auto_embed_using_no_provider",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED (a) USING".to_string(),
        ),
        (
            "auto_embed_model_no_string",
            "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED (a) USING openai MODEL".to_string(),
        ),
        // ---- HYBRID FROM malformed -----------------------------
        (
            "hybrid_no_fusion",
            "HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] LIMIT 10".to_string(),
        ),
        (
            "hybrid_no_vector_search",
            "HYBRID FROM hosts FUSION RERANK".to_string(),
        ),
        (
            "hybrid_unknown_fusion",
            "HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION GLORP LIMIT 10".to_string(),
        ),
        (
            "hybrid_search_neither_set",
            "SEARCH HYBRID COLLECTION c LIMIT 10".to_string(),
        ),
    ]
}

pub fn probabilistic_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // ----- CREATE envelope: missing names / EOF --------------
        ("prob_create_eof_after_keyword", "CREATE".to_string()),
        ("prob_create_hll_no_name", "CREATE HLL".to_string()),
        ("prob_create_sketch_no_name", "CREATE SKETCH".to_string()),
        ("prob_create_filter_no_name", "CREATE FILTER".to_string()),
        (
            "prob_create_unknown_kind",
            "CREATE BLOOM b1".to_string(),
        ),
        (
            "prob_create_hll_dangling_if",
            "CREATE HLL IF NOT EXISTS".to_string(),
        ),
        // ----- CREATE FILTER capacity edge cases -----------------
        (
            "prob_filter_capacity_no_value",
            "CREATE FILTER f1 CAPACITY".to_string(),
        ),
        (
            "prob_filter_capacity_negative",
            // Lexer emits Minus, then Integer; parse_integer errors.
            "CREATE FILTER f1 CAPACITY -1".to_string(),
        ),
        (
            "prob_filter_capacity_zero",
            "CREATE FILTER f1 CAPACITY 0".to_string(),
        ),
        (
            "prob_filter_capacity_non_numeric",
            "CREATE FILTER f1 CAPACITY many".to_string(),
        ),
        (
            "prob_filter_capacity_overflow",
            "CREATE FILTER f1 CAPACITY 99999999999999999999".to_string(),
        ),
        // ----- CREATE SKETCH width / depth edge cases ------------
        (
            "prob_sketch_width_no_value",
            "CREATE SKETCH s1 WIDTH".to_string(),
        ),
        (
            "prob_sketch_width_negative",
            "CREATE SKETCH s1 WIDTH -1".to_string(),
        ),
        (
            "prob_sketch_width_zero",
            "CREATE SKETCH s1 WIDTH 0".to_string(),
        ),
        (
            "prob_sketch_depth_no_value",
            "CREATE SKETCH s1 DEPTH".to_string(),
        ),
        (
            "prob_sketch_depth_negative",
            "CREATE SKETCH s1 DEPTH -1".to_string(),
        ),
        (
            "prob_sketch_depth_zero",
            // `Token::Depth` lexes as a keyword; the modifier loop
            // never matches, so `DEPTH 0` ends up as trailing
            // tokens and the top-level dispatcher errors.
            "CREATE SKETCH s1 DEPTH 0".to_string(),
        ),
        (
            "prob_sketch_width_then_depth",
            "CREATE SKETCH s1 WIDTH 100 DEPTH 5".to_string(),
        ),
        // ----- HLL operational surface ---------------------------
        ("prob_hll_eof_after_keyword", "HLL".to_string()),
        (
            "prob_hll_unknown_subcmd",
            "HLL FROBNICATE x".to_string(),
        ),
        ("prob_hll_add_no_name", "HLL ADD".to_string()),
        (
            "prob_hll_add_no_payload",
            "HLL ADD visitors".to_string(),
        ),
        (
            "prob_hll_add_unterminated_string",
            "HLL ADD visitors 'open".to_string(),
        ),
        (
            "prob_hll_count_no_name",
            "HLL COUNT".to_string(),
        ),
        (
            "prob_hll_merge_no_dest",
            "HLL MERGE".to_string(),
        ),
        // ----- SKETCH operational surface ------------------------
        ("prob_sketch_eof_after_keyword", "SKETCH".to_string()),
        ("prob_sketch_add_no_name", "SKETCH ADD".to_string()),
        (
            "prob_sketch_add_no_element",
            "SKETCH ADD events".to_string(),
        ),
        (
            "prob_sketch_add_unquoted_element",
            // ADD requires a string literal at the element slot.
            "SKETCH ADD events bareword".to_string(),
        ),
        (
            "prob_sketch_add_negative_count",
            "SKETCH ADD events 'click' -1".to_string(),
        ),
        (
            "prob_sketch_count_no_element",
            "SKETCH COUNT events".to_string(),
        ),
        // ----- FILTER operational surface ------------------------
        ("prob_filter_eof_after_keyword", "FILTER".to_string()),
        ("prob_filter_add_no_name", "FILTER ADD".to_string()),
        (
            "prob_filter_add_no_element",
            "FILTER ADD seen".to_string(),
        ),
        (
            "prob_filter_check_no_element",
            "FILTER CHECK seen".to_string(),
        ),
        (
            "prob_filter_delete_no_element",
            "FILTER DELETE seen".to_string(),
        ),
        (
            "prob_filter_count_no_name",
            "FILTER COUNT".to_string(),
        ),
        (
            "prob_filter_check_unquoted_element",
            "FILTER CHECK seen bareword".to_string(),
        ),
        // ----- DROP envelope -------------------------------------
        ("prob_drop_eof", "DROP".to_string()),
        (
            "prob_drop_unknown_kind",
            "DROP BLOOM b1".to_string(),
        ),
        (
            "prob_drop_hll_dangling_if",
            "DROP HLL IF EXISTS".to_string(),
        ),
        // ----- DoS / bytes-level shapes --------------------------
        (
            "prob_long_hll_name",
            format!("CREATE HLL {}", "h".repeat(10_000)),
        ),
        (
            "prob_filter_capacity_oversized",
            format!("CREATE FILTER f1 CAPACITY 1{}", "0".repeat(10_000)),
        ),
        (
            "prob_hll_add_oversized_payload",
            format!("HLL ADD visitors '{}'", "x".repeat(2 * 1024 * 1024)),
        ),
        (
            "prob_hll_add_many_elements",
            // 5_000 single-char string literals — exercises the
            // `HllAdd` accumulator loop without tripping the input
            // size guard.
            {
                let els: Vec<String> = (0..5_000).map(|_| "'x'".to_string()).collect();
                format!("HLL ADD visitors {}", els.join(" "))
            },
        ),
        (
            "prob_nul_byte_after_create",
            "CREATE HLL h1\0".to_string(),
        ),
        (
            "prob_garbage_after_filter_kw",
            "FILTER @#$%".to_string(),
        ),
        (
            "prob_unicode_in_element",
            "FILTER ADD seen '雪 ❄ ε≈μ'".to_string(),
        ),
        (
            "prob_zero_width_in_element",
            "HLL ADD visitors '\u{200b}\u{feff}user'".to_string(),
        ),
    ]
}

pub fn subquery_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        // ----- WHERE x IN (SELECT …) ----------------------------
        (
            "subq_in_eof_after_lparen",
            "SELECT * FROM t WHERE id IN (".to_string(),
        ),
        (
            "subq_in_select_eof_in_inner",
            "SELECT * FROM t WHERE id IN (SELECT".to_string(),
        ),
        (
            "subq_in_unbalanced_paren",
            "SELECT * FROM t WHERE id IN (SELECT id FROM u".to_string(),
        ),
        (
            "subq_in_dangling_comma_inner",
            "SELECT * FROM t WHERE id IN (SELECT id, FROM u)".to_string(),
        ),
        (
            "subq_not_in_subquery",
            "SELECT * FROM t WHERE id NOT IN (SELECT id FROM u)".to_string(),
        ),
        // ----- WHERE EXISTS (SELECT …) --------------------------
        (
            "subq_exists_eof_after_keyword",
            "SELECT * FROM t WHERE EXISTS".to_string(),
        ),
        (
            "subq_exists_no_paren",
            "SELECT * FROM t WHERE EXISTS SELECT id FROM u".to_string(),
        ),
        (
            "subq_exists_inner_garbage",
            "SELECT * FROM t WHERE EXISTS (@#$%)".to_string(),
        ),
        (
            "subq_not_exists_subquery",
            "SELECT * FROM t WHERE NOT EXISTS (SELECT id FROM u)".to_string(),
        ),
        // ----- scalar `= (SELECT …)` ----------------------------
        (
            "subq_scalar_eq_eof",
            "SELECT * FROM t WHERE x = (SELECT".to_string(),
        ),
        (
            "subq_scalar_eq_unterminated",
            "SELECT * FROM t WHERE x = (SELECT y FROM u".to_string(),
        ),
        (
            "subq_scalar_lt_subquery",
            "SELECT * FROM t WHERE x < (SELECT MAX(y) FROM u)".to_string(),
        ),
        // ----- FROM (SELECT …) AS sub ---------------------------
        (
            "subq_from_eof_after_lparen",
            "SELECT * FROM (".to_string(),
        ),
        (
            "subq_from_inner_not_select",
            "SELECT * FROM (DELETE FROM t) AS x".to_string(),
        ),
        (
            "subq_from_no_alias",
            "SELECT * FROM (SELECT id FROM t)".to_string(),
        ),
        (
            "subq_from_alias_no_as",
            "SELECT * FROM (SELECT id FROM t) sub".to_string(),
        ),
        (
            "subq_from_unterminated",
            "SELECT * FROM (SELECT id FROM t AS sub".to_string(),
        ),
        (
            "subq_from_double_subquery",
            "SELECT * FROM ((SELECT id FROM t) AS inner_q) AS outer_q".to_string(),
        ),
        // ----- correlated outer/inner refs ----------------------
        (
            "subq_correlated_outer_dot_col",
            "SELECT * FROM users u WHERE u.id IN (SELECT user_id FROM orders o WHERE o.user_id = u.id)".to_string(),
        ),
        (
            "subq_correlated_dangling_dot",
            "SELECT * FROM users u WHERE u.id IN (SELECT user_id FROM orders o WHERE o. = u.id)".to_string(),
        ),
        // ----- depth-guard pins (issue #91 SELECT-recursion) ---
        (
            "subq_deeply_nested_select_50",
            {
                let mut s = String::new();
                for _ in 0..50 {
                    s.push_str("(SELECT x FROM t WHERE x = ");
                }
                s.push('1');
                for _ in 0..50 {
                    s.push(')');
                }
                format!("SELECT * FROM t WHERE x = {}", s)
            },
        ),
        (
            "subq_deeply_nested_in_50",
            {
                let mut s = String::new();
                for _ in 0..50 {
                    s.push_str("SELECT x FROM t WHERE x IN (");
                }
                s.push_str("SELECT x FROM t");
                for _ in 0..50 {
                    s.push(')');
                }
                s
            },
        ),
        // ----- bytes-level adversarial --------------------------
        (
            "subq_in_oversized_inner",
            format!(
                "SELECT * FROM t WHERE id IN (SELECT {} FROM u)",
                "x".repeat(10_000),
            ),
        ),
        (
            "subq_nul_byte_inside_inner",
            "SELECT * FROM t WHERE id IN (SELECT id FROM u\0)".to_string(),
        ),
        (
            "subq_garbage_after_in",
            "SELECT * FROM t WHERE id IN @#$%".to_string(),
        ),
    ]
}

