use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::{BiometricFormat, BiometricSample, BitSignature, FeatureVector, Value};

const SPACE: [u8; 16] = [0x51; 16];

fn fingerprint() -> BiometricSample {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"FMR\0 20\0");
    bytes.extend_from_slice(&48_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(&[1, 0, 100, 3]);
    for (x, y, angle) in [(100_u16, 120_u16, 10_u8), (180, 160, 40), (130, 230, 90)] {
        bytes.extend_from_slice(&(x | 0x4000).to_be_bytes());
        bytes.extend_from_slice(&y.to_be_bytes());
        bytes.extend_from_slice(&[angle, 90]);
    }
    bytes.extend_from_slice(&[0, 0]);
    BiometricSample::with_format(BiometricFormat::FingerprintIso19794_2, bytes)
        .expect("standard fingerprint minutiae")
}

fn feature(values: [f32; 2]) -> Value {
    Value::FeatureVector(FeatureVector::new(SPACE, values.to_vec()).expect("feature vector"))
}

fn bits(value: u8) -> Value {
    Value::BitSignature(BitSignature::new(SPACE, vec![value], None).expect("bit signature"))
}

fn result_rows(result: reddb::runtime::RuntimeQueryResult) -> Vec<(u64, u64)> {
    result
        .result
        .records
        .iter()
        .map(|record| {
            let entity_id = match record.get("entity_id") {
                Some(Value::UnsignedInteger(value)) => *value,
                other => panic!("expected entity_id, got {other:?}"),
            };
            let score = match record.get("score") {
                Some(Value::Float(value)) => value.to_bits(),
                other => panic!("expected score, got {other:?}"),
            };
            (entity_id, score)
        })
        .collect()
}

fn assert_index_matches_exact(runtime: &RedDBRuntime, kind: &str, column: &str, query: Value) {
    let indexed = runtime
        .execute_query_with_params(
            &format!("SEARCH {kind} $1 IN identities COLUMN {column} LIMIT 10 MIN_SCORE 0.50"),
            std::slice::from_ref(&query),
        )
        .expect("indexed typed search");
    assert_eq!(indexed.notice, None, "typed index must be selected");
    let exact = runtime
        .execute_query_with_params(
            &format!(
                "SEARCH {kind} $1 IN identities COLUMN {column} LIMIT 10 MIN_SCORE 0.50 EXACT"
            ),
            &[query],
        )
        .expect("exact typed search");
    assert_eq!(result_rows(indexed), result_rows(exact));
}

fn assert_all_searches(runtime: &RedDBRuntime, print: &BiometricSample) {
    assert_index_matches_exact(runtime, "FEATURE", "face", feature([1.0, 0.0]));
    assert_index_matches_exact(runtime, "BITS", "iris", bits(0b1010_0000));
    assert_index_matches_exact(
        runtime,
        "FINGERPRINT",
        "print",
        Value::Fingerprint(print.clone()),
    );
}

#[test]
fn typed_similarity_indexes_rerank_and_survive_mutations_and_restart() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("typed-similarity.rdb");
    let print = fingerprint();
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        runtime
            .execute_query(
                "CREATE TABLE identities (id INTEGER, face FEATURE_VECTOR, iris BIT_SIGNATURE, print FINGERPRINT)",
            )
            .expect("table");
        for row in [
            vec![
                Value::Integer(1),
                feature([1.0, 0.0]),
                bits(0b1010_0000),
                Value::Fingerprint(print.clone()),
            ],
            vec![
                Value::Integer(2),
                feature([0.8, 0.2]),
                bits(0b1010_1000),
                Value::Fingerprint(print.clone()),
            ],
        ] {
            runtime
                .execute_query_with_params(
                    "INSERT INTO identities (id,face,iris,print) VALUES ($1,$2,$3,$4)",
                    &row,
                )
                .expect("initial row");
        }
        runtime
            .execute_query("CREATE INDEX identity_face_idx ON identities(face) USING HNSW")
            .expect("feature index");
        runtime
            .execute_query("CREATE INDEX identity_iris_idx ON identities(iris) USING HAMMING")
            .expect("bit index");
        runtime
            .execute_query("CREATE INDEX identity_print_idx ON identities(print) USING MINUTIAE")
            .expect("minutiae index");
        assert_all_searches(&runtime, &print);

        runtime
            .execute_query_with_params(
                "INSERT INTO identities (id,face,iris,print) VALUES ($1,$2,$3,$4)",
                &[
                    Value::Integer(3),
                    feature([0.2, 0.8]),
                    bits(0b0010_0000),
                    Value::Fingerprint(print.clone()),
                ],
            )
            .expect("indexed insert");
        assert_all_searches(&runtime, &print);

        runtime
            .execute_query_with_params(
                "UPDATE identities SET face = $1, iris = $2 WHERE id = 3",
                &[feature([0.9, 0.1]), bits(0b1010_0010)],
            )
            .expect("indexed update");
        runtime
            .execute_query("DELETE FROM identities WHERE id = 2")
            .expect("indexed delete");
        assert_all_searches(&runtime, &print);

        let incompatible = Value::FeatureVector(
            FeatureVector::new([0x52; 16], vec![1.0, 0.0]).expect("other feature space"),
        );
        let error = runtime
            .execute_query_with_params(
                "INSERT INTO identities (id,face,iris,print) VALUES ($1,$2,$3,$4)",
                &[
                    Value::Integer(4),
                    incompatible,
                    bits(0b1010_0000),
                    Value::Fingerprint(print.clone()),
                ],
            )
            .expect_err("index contract must reject another feature space");
        assert!(error.to_string().contains("incompatible"));
        assert!(runtime
            .execute_query("SELECT id FROM identities WHERE id = 4")
            .expect("rolled-back row lookup")
            .result
            .records
            .is_empty());

        let error = runtime
            .execute_query_with_params(
                "UPDATE identities SET face = $1 WHERE id = 3",
                &[Value::FeatureVector(
                    FeatureVector::new([0x52; 16], vec![0.9, 0.1]).expect("other feature space"),
                )],
            )
            .expect_err("index contract must reject an incompatible update");
        assert!(error.to_string().contains("incompatible"));
        let unchanged = runtime
            .execute_query("SELECT face FROM identities WHERE id = 3")
            .expect("updated row must be rolled back");
        assert_eq!(
            unchanged.result.records[0].get("face"),
            Some(&feature([0.9, 0.1]))
        );

        runtime
            .execute_query("INSERT INTO identities (id,face,iris,print) VALUES (5,NULL,NULL,NULL)")
            .expect("nullable typed columns");
        assert_all_searches(&runtime, &print);

        runtime.execute_query("BEGIN").expect("begin rollback case");
        runtime
            .execute_query_with_params(
                "INSERT INTO identities (id,face,iris,print) VALUES ($1,$2,$3,$4)",
                &[
                    Value::Integer(6),
                    feature([1.0, 0.0]),
                    bits(0b1010_0000),
                    Value::Fingerprint(print.clone()),
                ],
            )
            .expect("transactional indexed insert");
        runtime
            .execute_query_with_params(
                "UPDATE identities SET face = $1, iris = $2 WHERE id = 3",
                &[feature([0.0, 1.0]), bits(0b0101_0000)],
            )
            .expect("transactional indexed update");
        runtime
            .execute_query("DELETE FROM identities WHERE id = 1")
            .expect("transactional indexed delete");
        runtime.execute_query("ROLLBACK").expect("rollback");
        let rows = runtime
            .execute_query("SELECT id FROM identities WHERE id = 1 OR id = 3 OR id = 6 ORDER BY id")
            .expect("rows after rollback");
        assert_eq!(
            rows.result
                .records
                .iter()
                .map(|row| row.get("id"))
                .collect::<Vec<_>>(),
            vec![Some(&Value::Integer(1)), Some(&Value::Integer(3))]
        );
        assert_all_searches(&runtime, &print);
        runtime.checkpoint().expect("checkpoint");
    }

    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    assert_all_searches(&runtime, &print);
}
