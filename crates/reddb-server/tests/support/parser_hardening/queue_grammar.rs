//! Proptest strategies that emit syntactically valid Queue DSL
//! statements (issue #103).
//!
//! Mirrors the layout of `sql_grammar.rs` (#87) and
//! `migration_grammar.rs` (#88): each strategy returns a `String`
//! that, when fed through `parser::parse`, must not panic. The
//! valid-shape strategies must additionally succeed.
//!
//! The queue grammar covers the surface documented in
//! `crates/reddb-server/src/storage/query/parser/queue.rs`:
//!
//! - `CREATE QUEUE [IF NOT EXISTS] name [PRIORITY] [MAX_SIZE n]
//!    [MAX_ATTEMPTS n] [WITH TTL value unit] [WITH DLQ name]`
//! - `DROP QUEUE [IF EXISTS] name`
//! - `QUEUE PUSH queue value [PRIORITY n]`
//! - `QUEUE LPUSH queue value`
//! - `QUEUE RPUSH queue value [PRIORITY n]`
//! - `QUEUE POP queue [COUNT n]`
//! - `QUEUE LPOP|RPOP queue`
//! - `QUEUE PEEK queue [n]`
//! - `QUEUE LEN queue`
//! - `QUEUE PURGE queue`
//! - `QUEUE GROUP CREATE queue group`
//! - `QUEUE READ queue GROUP group [CONSUMER name] [COUNT n]`
//! - `QUEUE PENDING queue GROUP group`
//! - `QUEUE CLAIM queue GROUP group CONSUMER name MIN_IDLE n`
//! - `QUEUE ACK|NACK queue GROUP group 'message-id'`

use proptest::prelude::*;

/// Identifier suitable for queue / group / consumer names. Stays
/// well below the `max_identifier_chars` cap and avoids reserved
/// keywords by carrying the `q_` prefix.
pub fn ident() -> impl Strategy<Value = String> {
    "q_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// Single-quoted string payload (no embedded quotes).
pub fn string_payload() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _-]{0,16}".prop_map(|s| format!("'{}'", s))
}

/// Inline JSON literal payload covering the common `{key: value}`
/// shape the parser accepts via `parse_value`. Keys stay simple
/// identifiers and values are picked from a tiny safe set.
pub fn json_payload() -> impl Strategy<Value = String> {
    (
        "[a-z][a-z0-9_]{0,6}",
        prop_oneof![
            (0u32..1000).prop_map(|n| n.to_string()),
            "[a-zA-Z]{1,6}".prop_map(|s| format!("'{}'", s)),
            Just("true".to_string()),
            Just("false".to_string()),
            Just("null".to_string()),
        ],
    )
        .prop_map(|(k, v)| format!("{{{}: {}}}", k, v))
}

/// A queue PUSH payload: either a quoted string, an integer literal,
/// or an inline JSON object.
pub fn push_payload() -> impl Strategy<Value = String> {
    prop_oneof![
        string_payload(),
        (0u64..1_000_000).prop_map(|n| n.to_string()),
        json_payload(),
    ]
}

/// `MAX_SIZE n` clause where `n` is a small positive integer.
pub fn max_size_clause() -> impl Strategy<Value = String> {
    (1u64..1_000_000).prop_map(|n| format!("MAX_SIZE {}", n))
}

/// `MAX_ATTEMPTS n` clause.
pub fn max_attempts_clause() -> impl Strategy<Value = String> {
    (1u32..50).prop_map(|n| format!("MAX_ATTEMPTS {}", n))
}

/// `WITH TTL value unit` clause covering the documented duration
/// units (`ms`, `s`, `m`, `h`, `d`).
pub fn ttl_clause() -> impl Strategy<Value = String> {
    let unit = prop_oneof![
        Just("ms"),
        Just("s"),
        Just("m"),
        Just("h"),
        Just("d"),
    ];
    (1u64..1000, unit).prop_map(|(v, u)| format!("WITH TTL {} {}", v, u))
}

/// `WITH DLQ name` clause.
pub fn dlq_clause() -> impl Strategy<Value = String> {
    ident().prop_map(|n| format!("WITH DLQ {}", n))
}

/// `CREATE QUEUE [IF NOT EXISTS] name <clauses>`.
///
/// Strategy 1 of 5: covers the CREATE QUEUE surface end-to-end with
/// every optional clause exercised independently. The clause order
/// emitted here is deterministic but the parser accepts any order
/// (see `parse_create_queue_body` loop), so a future enhancement
/// can shuffle clauses without expanding the strategy count.
pub fn create_queue_stmt() -> impl Strategy<Value = String> {
    (
        any::<bool>(), // IF NOT EXISTS
        ident(),
        any::<bool>(),                        // PRIORITY
        proptest::option::of(max_size_clause()),
        proptest::option::of(max_attempts_clause()),
        proptest::option::of(ttl_clause()),
        proptest::option::of(dlq_clause()),
    )
        .prop_map(
            |(if_ne, name, priority, max_size, max_attempts, ttl, dlq)| {
                let mut s = String::from("CREATE QUEUE ");
                if if_ne {
                    s.push_str("IF NOT EXISTS ");
                }
                s.push_str(&name);
                if priority {
                    s.push_str(" PRIORITY");
                }
                if let Some(c) = max_size {
                    s.push(' ');
                    s.push_str(&c);
                }
                if let Some(c) = max_attempts {
                    s.push(' ');
                    s.push_str(&c);
                }
                if let Some(c) = ttl {
                    s.push(' ');
                    s.push_str(&c);
                }
                if let Some(c) = dlq {
                    s.push(' ');
                    s.push_str(&c);
                }
                s
            },
        )
}

/// `QUEUE PUSH name payload [PRIORITY n]` and the LPUSH/RPUSH
/// variants.
///
/// Strategy 2 of 5: exercises the PUSH surface. The optional
/// `PRIORITY n` modifier is generated for PUSH and RPUSH (LPUSH
/// does not accept it, mirroring the parser).
pub fn queue_push_stmt() -> impl Strategy<Value = String> {
    (
        prop_oneof![Just("PUSH"), Just("LPUSH"), Just("RPUSH")],
        ident(),
        push_payload(),
        proptest::option::of(0i32..100),
    )
        .prop_map(|(verb, name, payload, prio)| {
            let mut s = format!("QUEUE {} {} {}", verb, name, payload);
            // LPUSH does not accept a PRIORITY suffix in the parser,
            // so suppress it to keep the generator on the
            // valid-shape track.
            if verb != "LPUSH" {
                if let Some(p) = prio {
                    s.push_str(&format!(" PRIORITY {}", p));
                }
            }
            s
        })
}

/// `QUEUE POP name [COUNT n]` and the LPOP/RPOP aliases.
///
/// Strategy 3 of 5: exercises the POP surface. Only the canonical
/// `POP` verb accepts the `COUNT n` suffix in the parser, so the
/// alias variants emit no suffix.
pub fn queue_pop_stmt() -> impl Strategy<Value = String> {
    (
        prop_oneof![Just("POP"), Just("LPOP"), Just("RPOP")],
        ident(),
        proptest::option::of(1u32..100),
    )
        .prop_map(|(verb, name, count)| {
            let mut s = format!("QUEUE {} {}", verb, name);
            if verb == "POP" {
                if let Some(c) = count {
                    s.push_str(&format!(" COUNT {}", c));
                }
            }
            s
        })
}

/// Priority-modifier-focused strategy.
///
/// Strategy 4 of 5: pinpoints the `PRIORITY` modifier on
/// CREATE QUEUE and PUSH/RPUSH, plus the bare priority integer
/// after PUSH. Keeping a dedicated strategy makes proptest
/// shrinking land directly on the priority token when the modifier
/// regresses.
pub fn priority_modifier_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        // CREATE QUEUE name PRIORITY
        ident().prop_map(|n| format!("CREATE QUEUE {} PRIORITY", n)),
        // CREATE QUEUE name MAX_SIZE k PRIORITY
        (ident(), 1u64..10_000).prop_map(|(n, k)| {
            format!("CREATE QUEUE {} MAX_SIZE {} PRIORITY", n, k)
        }),
        // QUEUE PUSH name 'x' PRIORITY n
        (ident(), 0i32..100).prop_map(|(n, p)| {
            format!("QUEUE PUSH {} 'x' PRIORITY {}", n, p)
        }),
        // QUEUE RPUSH name 'x' PRIORITY n
        (ident(), 0i32..100).prop_map(|(n, p)| {
            format!("QUEUE RPUSH {} 'x' PRIORITY {}", n, p)
        }),
    ]
}

/// Consumer-group syntax surface.
///
/// Strategy 5 of 5: every queue command that mentions a group or
/// consumer (`GROUP CREATE`, `READ ... GROUP ... CONSUMER ...`,
/// `PENDING`, `CLAIM`, `ACK`, `NACK`).
pub fn consumer_group_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        // GROUP CREATE
        (ident(), ident()).prop_map(|(q, g)| format!("QUEUE GROUP CREATE {} {}", q, g)),
        // READ ... GROUP g CONSUMER c [COUNT n]
        (ident(), ident(), ident(), proptest::option::of(1u32..50)).prop_map(
            |(q, g, c, n)| {
                let mut s = format!("QUEUE READ {} GROUP {} CONSUMER {}", q, g, c);
                if let Some(cnt) = n {
                    s.push_str(&format!(" COUNT {}", cnt));
                }
                s
            },
        ),
        // PENDING
        (ident(), ident()).prop_map(|(q, g)| format!("QUEUE PENDING {} GROUP {}", q, g)),
        // CLAIM ... CONSUMER c MIN_IDLE n
        (ident(), ident(), ident(), 0u64..600_000).prop_map(|(q, g, c, m)| {
            format!(
                "QUEUE CLAIM {} GROUP {} CONSUMER {} MIN_IDLE {}",
                q, g, c, m
            )
        }),
        // ACK / NACK
        (
            prop_oneof![Just("ACK"), Just("NACK")],
            ident(),
            ident(),
            "[a-z0-9-]{1,16}",
        )
            .prop_map(|(verb, q, g, mid)| {
                format!("QUEUE {} {} GROUP {} '{}'", verb, q, g, mid)
            }),
    ]
}

/// Top-level union: any of the queue grammar shapes covered above.
pub fn any_queue_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        create_queue_stmt(),
        queue_push_stmt(),
        queue_pop_stmt(),
        priority_modifier_stmt(),
        consumer_group_stmt(),
    ]
}
