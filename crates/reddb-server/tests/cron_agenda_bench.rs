//! Reproducible SQL comparison. Run with cargo test --profile release-fast
//! -p reddb-io-server --test cron_agenda_bench -- --ignored --nocapture.
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::time::Instant;

const ROOMS: i64 = 8;
const RULES_PER_ROOM: i64 = 4;
const MINUTE: i64 = 60_000;
const DAY: i64 = 1440 * MINUTE;
const DAY_START: i64 = 1_798_761_600_000; // 2027-01-01 00:00 UTC
const SAMPLES: usize = 31;

fn sql(rt: &RedDBRuntime, query: &str) {
    rt.execute_query(query)
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn load(rt: &RedDBRuntime, dense: bool) -> usize {
    sql(rt, "SET CONFIG runtime.result_cache.enabled = false");
    sql(rt, "CREATE TABLE jobs (id INTEGER PRIMARY KEY, room INTEGER, schedule CRON, duration_ms INTEGER)");
    sql(
        rt,
        "CREATE TABLE slots (id INTEGER, room INTEGER, at_time BIGINT, ends_ms BIGINT)",
    );
    let mut slots = Vec::new();
    for room in 0..ROOMS {
        for rule in 0..RULES_PER_ROOM {
            let id = room * RULES_PER_ROOM + rule;
            let duration = [45, 15, 30, 5][rule as usize] * MINUTE;
            let every_minute = dense && rule == 3;
            let (hour, minute) = [(23, 45), (9, 0), (11, 0), (16, 15)][rule as usize];
            let cron = if every_minute {
                "* * * * *".to_string()
            } else {
                format!("{minute} {hour} * * *")
            };
            sql(rt, &format!("INSERT INTO jobs (id, room, schedule, duration_ms) VALUES ({id}, {room}, '{cron}', {duration})"));
            // Independent oracle for the two-day cache (including carry-in).
            for day in [-1, 0] {
                for slot in 0..1440 {
                    if every_minute || slot == hour * 60 + minute {
                        let start = DAY_START + day * DAY + slot * MINUTE;
                        slots.push(format!("({id},{room},{start},{})", start + duration));
                    }
                }
            }
        }
    }
    for batch in slots.chunks(100) {
        sql(
            rt,
            &format!(
                "INSERT INTO slots (id, room, at_time, ends_ms) VALUES {}",
                batch.join(",")
            ),
        );
    }
    sql(rt, "CREATE INDEX rules_by_room ON jobs (room) USING BTREE");
    sql(
        rt,
        "CREATE INDEX slots_by_room ON slots (room, at_time) USING BTREE",
    );
    sql(
        rt,
        "CREATE INDEX slots_by_time ON slots (at_time) USING BTREE",
    );
    slots.len()
}

fn measure(
    rt: &RedDBRuntime,
    rule_sql: &str,
    slot_sql: &str,
    start: i64,
    end: i64,
) -> (usize, u128, u128) {
    let mut samples = [Vec::new(), Vec::new()];
    let mut count = 0;
    for pass in 0..SAMPLES + 2 {
        let mut answers = [Vec::new(), Vec::new()];
        // Alternate order, with two warmup pairs outside the samples.
        for index in [pass % 2, 1 - pass % 2] {
            let (query, args) = if index == 0 {
                (
                    rule_sql,
                    [Value::TimestampMs(start), Value::TimestampMs(end)],
                )
            } else {
                (slot_sql, [Value::BigInt(start), Value::BigInt(end)])
            };
            let before = Instant::now();
            let result = rt
                .execute_query_with_params(query, &args)
                .unwrap_or_else(|e| panic!("{query}: {e}"));
            let elapsed = before.elapsed().as_micros();
            if pass >= 2 {
                samples[index].push(elapsed);
            }
            for row in &result.result.records {
                let Some(Value::Integer(id)) = row.get("id") else {
                    panic!("unexpected job ID {row:?}")
                };
                let Some(Value::TimestampMs(at) | Value::BigInt(at) | Value::Integer(at)) =
                    row.get("at_time")
                else {
                    panic!("unexpected start {row:?}")
                };
                answers[index].push((*id, *at));
            }
        }
        assert_eq!(
            answers[0], answers[1],
            "strategies must return the same ordered occurrences"
        );
        count = answers[0].len();
    }
    let [rules, slots] = samples.each_mut().map(|sample| {
        sample.sort_unstable();
        sample[(SAMPLES * 95).div_ceil(100) - 1]
    });
    (count, rules, slots)
}

#[test]
#[ignore = "local SQL latency and logical index-entry measurement"]
fn agenda_queries() {
    for dense in [false, true] {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("memory runtime");
        let slot_count = load(&rt, dense);
        println!(
            "dense={dense} rules={} slots={slot_count} rule_index_entries={} slot_index_entries={}",
            ROOMS * RULES_PER_ROOM,
            ROOMS * RULES_PER_ROOM,
            slot_count * 2
        );
        for (name, resource, conflict) in [
            ("room_day", true, false),
            ("first_conflict", true, true),
            ("all_day", false, false),
        ] {
            let where_room = if resource { "WHERE room = 2" } else { "" };
            let window_start = if conflict {
                "$1 - duration_ms + 1"
            } else {
                "$1"
            };
            let rule_sql = format!("SELECT id, cron_occurrences(schedule, {window_start}, $2) AS at_time FROM jobs {where_room} ORDER BY at_time, id");
            let scope = if resource { "room = 2 AND " } else { "" };
            let lower = if conflict {
                "ends_ms > $1"
            } else {
                "at_time >= $1"
            };
            let slot_sql = format!("SELECT id, at_time FROM slots WHERE {scope}{lower} AND at_time < $2 ORDER BY at_time, id");
            let limit = if conflict { " LIMIT 1" } else { "" };
            let end = DAY_START + if conflict { 10 * MINUTE } else { DAY };
            let (count, rule_p95, slot_p95) = measure(
                &rt,
                &(rule_sql + limit),
                &(slot_sql + limit),
                DAY_START,
                end,
            );
            let expected = if conflict {
                1
            } else {
                let per_room = if dense { 1443 } else { 4 };
                if resource {
                    per_room
                } else {
                    ROOMS as usize * per_room
                }
            };
            assert_eq!(count, expected);
            let rules = rt
                .execute_query(&format!("SELECT id FROM jobs {where_room}"))
                .expect("count the input rows admitted by the resource filter")
                .result
                .records
                .len();
            println!("{name}: rows={count} candidate_rules={rules} expanded_p95_us={rule_p95} materialized_p95_us={slot_p95}");
        }
    }
}
