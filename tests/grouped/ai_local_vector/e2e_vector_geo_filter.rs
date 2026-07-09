use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

#[derive(Clone, Copy)]
enum TestLocation {
    Geo { lat: f64, lon: f64 },
    Missing,
    NonGeo,
}

struct Restaurant {
    content: &'static str,
    dense: [f32; 2],
    cuisine: &'static str,
    location: TestLocation,
}

const CENTER_LAT: f64 = 48.8566;
const CENTER_LON: f64 = 2.3522;
const RADIUS_KM: f64 = 5.0;

const CORPUS: &[Restaurant] = &[
    Restaurant {
        content: "outside-perfect",
        dense: [1.0, 0.0],
        cuisine: "bistro",
        location: TestLocation::Geo {
            lat: 41.9028,
            lon: 12.4964,
        },
    },
    Restaurant {
        content: "inside-best",
        dense: [0.99, 0.01],
        cuisine: "bistro",
        location: TestLocation::Geo {
            lat: 48.8566,
            lon: 2.3522,
        },
    },
    Restaurant {
        content: "non-geo-perfect",
        dense: [1.0, 0.0],
        cuisine: "bistro",
        location: TestLocation::NonGeo,
    },
    Restaurant {
        content: "missing-perfect",
        dense: [1.0, 0.0],
        cuisine: "bistro",
        location: TestLocation::Missing,
    },
    Restaurant {
        content: "wrong-cuisine-perfect",
        dense: [1.0, 0.0],
        cuisine: "cafe",
        location: TestLocation::Geo {
            lat: 48.8566,
            lon: 2.3522,
        },
    },
    Restaurant {
        content: "inside-second",
        dense: [0.95, 0.05],
        cuisine: "bistro",
        location: TestLocation::Geo {
            lat: 48.8606,
            lon: 2.3376,
        },
    },
    Restaurant {
        content: "inside-third",
        dense: [0.8, 0.6],
        cuisine: "bistro",
        location: TestLocation::Geo {
            lat: 48.8584,
            lon: 2.2945,
        },
    },
];

fn runtime(indexed: bool) -> RedDBRuntime {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE VECTOR restaurants DIM 2 METRIC cosine")
        .expect("create vector collection");
    for row in CORPUS {
        rt.execute_query(&insert_sql(row)).unwrap_or_else(|err| {
            panic!("insert {} should succeed: {err:?}", row.content);
        });
    }
    if indexed {
        rt.execute_query("CREATE INDEX idx_restaurant_location ON restaurants (location) USING H3")
            .expect("create h3 index");
    }
    rt
}

fn insert_sql(row: &Restaurant) -> String {
    let metadata = match row.location {
        TestLocation::Geo { lat, lon } => {
            format!(
                r#"{{"location":{{"lat":{lat},"lon":{lon}}},"cuisine":"{}"}}"#,
                row.cuisine
            )
        }
        TestLocation::Missing => format!(r#"{{"cuisine":"{}"}}"#, row.cuisine),
        TestLocation::NonGeo => {
            format!(r#"{{"location":"not-geo","cuisine":"{}"}}"#, row.cuisine)
        }
    };
    format!(
        "INSERT INTO restaurants VECTOR (dense, content, metadata) \
         VALUES ([{}, {}], '{}', '{}')",
        row.dense[0], row.dense[1], row.content, metadata
    )
}

fn geo_query(limit: usize) -> String {
    format!(
        "VECTOR SEARCH restaurants SIMILAR TO [1.0, 0.0] \
         WHERE GEO_DISTANCE(location, {CENTER_LAT}, {CENTER_LON}) <= {RADIUS_KM} \
         AND cuisine = 'bistro' LIMIT {limit}"
    )
}

fn hit_names(rt: &RedDBRuntime, sql: &str) -> Vec<String> {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\n{err:?}"))
        .result
        .records
        .iter()
        .map(|record| match record.get("content") {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected content text, got {other:?}"),
        })
        .collect()
}

fn exhaustive_oracle(limit: usize) -> Vec<String> {
    let mut scored = CORPUS
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            if row.cuisine != "bistro" {
                return None;
            }
            let TestLocation::Geo { lat, lon } = row.location else {
                return None;
            };
            if reddb::geo::haversine_km(CENTER_LAT, CENTER_LON, lat, lon) > RADIUS_KM {
                return None;
            }
            Some((index, cosine_score(row.dense), row.content))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, content)| content.to_string())
        .collect()
}

fn cosine_score(vector: [f32; 2]) -> f32 {
    let norm = (vector[0] * vector[0] + vector[1] * vector[1]).sqrt();
    if norm == 0.0 {
        0.0
    } else {
        vector[0] / norm
    }
}

#[test]
fn vector_geo_radius_filter_scores_only_entities_inside_radius() {
    let expected = exhaustive_oracle(2);
    let full_scan = runtime(false);
    let indexed = runtime(true);

    let full_scan_hits = hit_names(&full_scan, &geo_query(2));
    let indexed_hits = hit_names(&indexed, &geo_query(2));

    assert_eq!(full_scan_hits, expected);
    assert_eq!(indexed_hits, full_scan_hits);
    assert!(!full_scan_hits.contains(&"missing-perfect".to_string()));
    assert!(!full_scan_hits.contains(&"non-geo-perfect".to_string()));
}

#[test]
fn vector_search_without_geo_filter_keeps_existing_metadata_filter_behavior() {
    let rt = runtime(false);
    let hits = hit_names(
        &rt,
        "VECTOR SEARCH restaurants SIMILAR TO [1.0, 0.0] \
         WHERE cuisine = 'bistro' LIMIT 3",
    );
    assert!(hits.contains(&"outside-perfect".to_string()));
    assert!(hits.contains(&"missing-perfect".to_string()));
    assert!(hits.contains(&"non-geo-perfect".to_string()));
}
