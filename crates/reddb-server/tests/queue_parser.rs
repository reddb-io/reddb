//! Parser hardening test suite for the Queue DSL (issue #103).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover the `CREATE QUEUE`, `DROP QUEUE`, `QUEUE PUSH/POP/PEEK/...`,
//! and consumer-group surfaces. The queue parser is reached through
//! the standard `reddb_server::storage::query::parser::parse` entry
//! point, so `ParserLimits` (max_depth / max_input_bytes /
//! max_identifier_chars) cascade automatically — this file pins the
//! contract.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::queue_adversarial_inputs, queue_grammar,
    HardenedParser,
};

/// `HardenedParser` shim around the queue DSL surface. Like the
/// migration shim, the queue parser shares the top-level
/// `parser::parse` entry point; the property + snapshot suites
/// below are what differentiate this from the SQL shim by feeding
/// only queue-shaped inputs.
pub struct QueueParser;

impl HardenedParser for QueueParser {
    type Error = ParseError;

    fn parse(input: &str) -> Result<(), Self::Error> {
        parser::parse(input).map(|_| ())
    }

    fn parse_with_limits(input: &str, limits: ParserLimits) -> Result<(), Self::Error> {
        let mut p = parser::Parser::with_limits(input, limits)?;
        p.parse().map(|_| ())
    }
}

// ---- panic-safety on adversarial corpus -------------------------

#[test]
fn queue_parser_does_not_panic_on_adversarial_corpus() {
    // Bigger stack: a couple of corpus entries probe deep recursion
    // limits and the default 2 MiB test thread stack runs them too
    // close to the line.
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in queue_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<QueueParser>(&input);
                }));
                if result.is_err() {
                    panic!("queue adversarial corpus entry {} panicked", name);
                }
            }
        })
        .expect("spawn corpus thread");
    handle.join().expect("corpus thread panic");
}

// ---- property tests ---------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    })]

    /// Generated CREATE QUEUE shapes parse cleanly.
    #[test]
    fn proptest_create_queue_roundtrips(s in queue_grammar::create_queue_stmt()) {
        harness::roundtrip_property::<QueueParser>(&s);
        prop_assert!(
            QueueParser::parse(&s).is_ok(),
            "create queue did not parse: {}", s
        );
    }

    /// Generated QUEUE PUSH shapes parse cleanly.
    #[test]
    fn proptest_queue_push_roundtrips(s in queue_grammar::queue_push_stmt()) {
        harness::roundtrip_property::<QueueParser>(&s);
        prop_assert!(
            QueueParser::parse(&s).is_ok(),
            "queue push did not parse: {}", s
        );
    }

    /// Generated QUEUE POP shapes parse cleanly.
    #[test]
    fn proptest_queue_pop_roundtrips(s in queue_grammar::queue_pop_stmt()) {
        harness::roundtrip_property::<QueueParser>(&s);
        prop_assert!(
            QueueParser::parse(&s).is_ok(),
            "queue pop did not parse: {}", s
        );
    }

    /// Generated PRIORITY-modifier shapes parse cleanly. Pinned as
    /// its own strategy so a regression in the modifier shrinks
    /// directly to the `PRIORITY` token.
    #[test]
    fn proptest_priority_modifier_roundtrips(s in queue_grammar::priority_modifier_stmt()) {
        harness::roundtrip_property::<QueueParser>(&s);
        prop_assert!(
            QueueParser::parse(&s).is_ok(),
            "priority modifier did not parse: {}", s
        );
    }

    /// Generated consumer-group shapes (GROUP CREATE / READ /
    /// PENDING / CLAIM / ACK / NACK) parse cleanly.
    #[test]
    fn proptest_consumer_group_roundtrips(s in queue_grammar::consumer_group_stmt()) {
        harness::roundtrip_property::<QueueParser>(&s);
        prop_assert!(
            QueueParser::parse(&s).is_ok(),
            "consumer group syntax did not parse: {}", s
        );
    }

    /// Arbitrary bytes prefixed with a queue keyword never panic —
    /// `Err` is fine, panic is not.
    #[test]
    fn proptest_queue_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("CREATE QUEUE ".to_string()),
            Just("DROP QUEUE ".to_string()),
            Just("QUEUE PUSH ".to_string()),
            Just("QUEUE POP ".to_string()),
            Just("QUEUE GROUP CREATE ".to_string()),
            Just("QUEUE READ ".to_string()),
            Just("QUEUE CLAIM ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<QueueParser>(&s);
    }

    /// Tighter limits always refuse oversized PUSH payloads.
    #[test]
    fn proptest_queue_push_input_size_limit_enforced(len in 200usize..2000) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let payload = "x".repeat(len);
        let input = format!("QUEUE PUSH q '{}'", payload);
        let r = QueueParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized push payload must error");
    }
}

// ---- happy-path regression tests --------------------------------
//
// These pin the well-formed queue shapes that the README and parser
// unit tests advertise. They live here (rather than as another set
// of unit tests inside the parser crate) so the integration
// surface is exercised end-to-end through `parser::parse` and the
// AST contract is observable from the consumer side.

use reddb_server::storage::query::ast::{QueryExpr, QueueCommand, QueueSide};

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn create_queue_with_max_size_and_priority_parses() {
    let q = parse_query("CREATE QUEUE tasks MAX_SIZE 1000 PRIORITY");
    match q {
        QueryExpr::CreateQueue(cq) => {
            assert_eq!(cq.name, "tasks");
            assert_eq!(cq.max_size, Some(1000));
            assert!(cq.priority);
            assert_eq!(cq.max_attempts, 3);
            assert!(cq.dlq.is_none());
        }
        other => panic!("expected CreateQueue, got {other:?}"),
    }
}

#[test]
fn create_queue_with_dlq_and_max_attempts_parses() {
    let q = parse_query("CREATE QUEUE tasks WITH DLQ failed MAX_ATTEMPTS 5");
    match q {
        QueryExpr::CreateQueue(cq) => {
            assert_eq!(cq.name, "tasks");
            assert_eq!(cq.dlq.as_deref(), Some("failed"));
            assert_eq!(cq.max_attempts, 5);
        }
        other => panic!("expected CreateQueue, got {other:?}"),
    }
}

#[test]
fn create_queue_if_not_exists_sets_flag() {
    let q = parse_query("CREATE QUEUE IF NOT EXISTS tasks");
    match q {
        QueryExpr::CreateQueue(cq) => {
            assert!(cq.if_not_exists, "IF NOT EXISTS flag must be set");
            assert_eq!(cq.name, "tasks");
        }
        other => panic!("expected CreateQueue, got {other:?}"),
    }
}

#[test]
fn queue_push_string_payload_parses() {
    let q = parse_query("QUEUE PUSH tasks 'hello world'");
    match q {
        QueryExpr::QueueCommand(QueueCommand::Push {
            queue,
            side,
            priority,
            ..
        }) => {
            assert_eq!(queue, "tasks");
            // Default PUSH targets the right side.
            assert_eq!(side, QueueSide::Right);
            assert_eq!(priority, None);
        }
        other => panic!("expected QueueCommand::Push, got {other:?}"),
    }
}

#[test]
fn queue_push_with_priority_modifier_parses() {
    let q = parse_query("QUEUE PUSH tasks 'x' PRIORITY 7");
    match q {
        QueryExpr::QueueCommand(QueueCommand::Push { priority, .. }) => {
            assert_eq!(priority, Some(7));
        }
        other => panic!("expected QueueCommand::Push, got {other:?}"),
    }
}

#[test]
fn queue_pop_with_count_parses() {
    let q = parse_query("QUEUE POP tasks COUNT 5");
    match q {
        QueryExpr::QueueCommand(QueueCommand::Pop { queue, count, side }) => {
            assert_eq!(queue, "tasks");
            assert_eq!(count, 5);
            // Default POP pulls from the left side.
            assert_eq!(side, QueueSide::Left);
        }
        other => panic!("expected QueueCommand::Pop, got {other:?}"),
    }
}

#[test]
fn queue_lpush_rpop_aliases_set_side() {
    match parse_query("QUEUE LPUSH tasks 'left'") {
        QueryExpr::QueueCommand(QueueCommand::Push { side, .. }) => {
            assert_eq!(side, QueueSide::Left);
        }
        other => panic!("expected Push, got {other:?}"),
    }
    match parse_query("QUEUE RPOP tasks") {
        QueryExpr::QueueCommand(QueueCommand::Pop { side, .. }) => {
            assert_eq!(side, QueueSide::Right);
        }
        other => panic!("expected Pop, got {other:?}"),
    }
}

#[test]
fn queue_group_create_parses() {
    let q = parse_query("QUEUE GROUP CREATE tasks workers");
    match q {
        QueryExpr::QueueCommand(QueueCommand::GroupCreate { queue, group }) => {
            assert_eq!(queue, "tasks");
            assert_eq!(group, "workers");
        }
        other => panic!("expected GroupCreate, got {other:?}"),
    }
}

#[test]
fn queue_claim_full_shape_parses() {
    let q = parse_query("QUEUE CLAIM tasks GROUP workers CONSUMER worker2 MIN_IDLE 60000");
    match q {
        QueryExpr::QueueCommand(QueueCommand::Claim {
            queue,
            group,
            consumer,
            min_idle_ms,
        }) => {
            assert_eq!(queue, "tasks");
            assert_eq!(group, "workers");
            assert_eq!(consumer, "worker2");
            assert_eq!(min_idle_ms, 60_000);
        }
        other => panic!("expected Claim, got {other:?}"),
    }
}

#[test]
fn drop_queue_if_exists_parses() {
    let q = parse_query("DROP QUEUE IF EXISTS tasks");
    match q {
        QueryExpr::DropQueue(dq) => {
            assert_eq!(dq.name, "tasks");
            assert!(dq.if_exists, "IF EXISTS flag must propagate");
        }
        other => panic!("expected DropQueue, got {other:?}"),
    }
}
