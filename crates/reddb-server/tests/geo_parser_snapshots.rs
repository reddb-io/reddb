//! Pinned geo / spatial parse-error snapshots (issue #104).
//!
//! Mirrors `parser_snapshots.rs` and `migration_parser_snapshots.rs`
//! for the SEARCH SPATIAL grammar, the RTREE index method, and the
//! geo scalar functions. Each test in this file calls
//! `fmt_parse_error` on a hand-crafted bad input; snapshot files
//! live in `tests/snapshots/`.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review`.
//!   - CI: snapshots must match exactly.
//!
//! Every test installs the shared secret redactor (issue #98) before
//! the snapshot is written, so even if a fuzzer-found input embeds a
//! token-shaped substring the resulting `*.snap` file is clean.

mod support {
    pub mod parser_hardening;
}

use reddb_server::storage::query::parser;
use support::parser_hardening::secret_redactor;

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses render as `UNEXPECTED OK` so a missing error
/// path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _guard = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- SEARCH SPATIAL RADIUS error scenarios ---------------------

snap!(geo_radius_eof_after_keyword, "SEARCH SPATIAL RADIUS");
snap!(
    geo_radius_eof_after_lat_lon,
    "SEARCH SPATIAL RADIUS 48.8566 2.3522"
);
snap!(
    geo_radius_missing_collection,
    "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 sites COLUMN location"
);
snap!(
    geo_radius_missing_column_kw,
    "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION sites location"
);
snap!(
    geo_radius_garbage_after_radius_kw,
    "SEARCH SPATIAL RADIUS @#$% COLLECTION c COLUMN col"
);
snap!(
    geo_radius_negative_radius_no_unary_minus,
    "SEARCH SPATIAL RADIUS 0.0 0.0 -10.0 COLLECTION c COLUMN col"
);
snap!(
    geo_radius_nan_literal_at_lat,
    "SEARCH SPATIAL RADIUS NaN 0.0 10.0 COLLECTION c COLUMN col"
);

// ----- SEARCH SPATIAL NEAREST error scenarios --------------------

snap!(geo_nearest_eof_after_keyword, "SEARCH SPATIAL NEAREST");
snap!(
    geo_nearest_missing_k_keyword,
    "SEARCH SPATIAL NEAREST 0.0 0.0 5 COLLECTION sites COLUMN location"
);
snap!(
    geo_nearest_eof_after_k,
    "SEARCH SPATIAL NEAREST 0.0 0.0 K"
);
snap!(
    geo_nearest_negative_k_no_unary_minus,
    "SEARCH SPATIAL NEAREST 0.0 0.0 K -1 COLLECTION sites COLUMN location"
);

// ----- SEARCH SPATIAL BBOX error scenarios -----------------------

snap!(geo_bbox_eof_after_keyword, "SEARCH SPATIAL BBOX");
snap!(
    geo_bbox_eof_mid_corners,
    "SEARCH SPATIAL BBOX 0.0 0.0"
);

// ----- SEARCH SPATIAL dispatcher errors --------------------------

snap!(
    geo_unknown_subcommand,
    "SEARCH SPATIAL POLYGON 0.0 0.0 COLLECTION c COLUMN col"
);
snap!(geo_eof_after_search_spatial, "SEARCH SPATIAL");

// ----- RTREE index DDL error scenarios ---------------------------

snap!(
    geo_rtree_unknown_method,
    "CREATE INDEX gix ON sites (location) USING WRONGTREE"
);
snap!(
    geo_rtree_no_columns,
    "CREATE INDEX gix ON sites () USING RTREE"
);

// ----- Distance fn error scenarios -------------------------------

snap!(geo_distance_no_args, "SELECT GEO_DISTANCE() FROM t");
snap!(
    geo_haversine_dangling_comma,
    "SELECT HAVERSINE(0.0, 0.0, 1.0,) FROM t"
);
