//! Guard for the cargo-nextest adoption (issue #973): pin the per-test timeout,
//! the lib/e2e lane definitions, and the shardability of the e2e lane so they
//! cannot silently regress.

const NEXTEST_CONFIG: &str = include_str!("../.config/nextest.toml");
const SHARD_SCRIPT: &str = include_str!("../scripts/nextest-e2e-shard.sh");
const MAKEFILE: &str = include_str!("../Makefile");
const LANES_DOC: &str = include_str!("../docs/testing/nextest-lanes.md");

fn assert_contains(haystack: &str, needle: &str) {
    assert!(haystack.contains(needle), "expected to contain: {needle}");
}

#[test]
fn nextest_config_enforces_a_per_test_timeout_that_kills_hung_tests() {
    // A hung test must be terminated and reported, not left to stall the run.
    assert_contains(NEXTEST_CONFIG, "slow-timeout");
    assert_contains(NEXTEST_CONFIG, "terminate-after");
    assert_contains(NEXTEST_CONFIG, "[profile.default]");
}

#[test]
fn nextest_config_defines_a_ci_profile_with_junit_output() {
    assert_contains(NEXTEST_CONFIG, "[profile.ci]");
    assert_contains(NEXTEST_CONFIG, "[profile.ci.junit]");
}

#[test]
fn lib_and_e2e_lanes_are_defined_and_runnable_separately() {
    // Lib lane: fast in-crate unit tests.
    assert_contains(MAKEFILE, "test-nextest-lib:");
    assert_contains(MAKEFILE, "--lib");
    // e2e lane: the integration-test binaries, selected by the `kind(test)` filterset.
    assert_contains(MAKEFILE, "test-nextest-e2e:");
    assert_contains(MAKEFILE, "kind(test)");
}

#[test]
fn e2e_lane_is_shardable_across_runners() {
    // The shard helper partitions the e2e lane deterministically across runners.
    assert_contains(SHARD_SCRIPT, "kind(test)");
    assert_contains(SHARD_SCRIPT, "--partition");
    assert_contains(SHARD_SCRIPT, "count:${INDEX}/${TOTAL}");
}

#[test]
fn lanes_are_documented() {
    assert_contains(LANES_DOC, "Lib lane");
    assert_contains(LANES_DOC, "e2e lane");
    assert_contains(LANES_DOC, "--partition count:<index>/<total>");
}
