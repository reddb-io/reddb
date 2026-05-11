import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("Blob Cache runtime evidence covers L1, TTL, invalidation, L2, and synopsis behavior", () => {
  const tests = read("crates/reddb-server/src/storage/cache/blob/cache/tests.rs");

  for (const symbol of [
    "fn put_get_and_exists_round_trip_blob()",
    "fn missing_key_updates_miss_counter()",
    "fn byte_capacity_evicts_with_sieve()",
    "fn hard_ttl_expires_entries_on_get_and_exists()",
    "fn absolute_expiry_is_hard_boundary()",
    "fn l1_admission_never_accepts_put_without_storing_l1_entry()",
    "fn priority_biases_sieve_eviction_toward_lower_priority_entries()",
    "fn invalidate_key_removes_one_entry_and_is_idempotent()",
    "fn invalidate_prefix_removes_matching_namespace_keys_only()",
    "fn invalidate_tag_and_dependency_use_indexes()",
    "fn namespace_flush_bumps_generation_and_old_entries_are_immediately_absent()",
    "fn l2_rehydrates_after_reopen_without_json_rows()",
    "fn l2_expired_entry_does_not_rehydrate_on_reopen()",
    "fn l2_invalidated_entry_does_not_resurrect_after_reopen()",
    "fn l2_metadata_last_hides_partial_blob_after_fault()",
    "fn l2_synopsis_negative_skip_avoids_metadata_read()",
    "fn l2_synopsis_maybe_present_verifies_authoritative_metadata()",
    "fn stale_synopsis_bits_after_delete_cannot_produce_present()",
    "fn stale_synopsis_bits_after_expiry_cannot_produce_present()",
    "fn l2_synopsis_rebuilds_from_metadata_on_reopen()",
  ]) {
    assert.match(tests, new RegExp(symbol.replace(/[()]/g, "\\$&")));
  }
});

test("Blob Cache result-cache adapter evidence is separated from missing warm-restart contract", () => {
  const frameTests = read("crates/reddb-server/src/runtime/statement_frame.rs");
  const runtime = read("crates/reddb-server/src/runtime/impl_core.rs");
  const followUp = read("issues/348-result-cache-l2-warm-restart-contract.md");

  assert.match(frameTests, /fn blob_cache_backend_populates_blob_path_without_legacy_write\(\)/);
  assert.match(frameTests, /fn blob_cache_backend_keeps_volatile_select_out_of_blob_path\(\)/);
  assert.match(frameTests, /fn shadow_backend_dual_writes_and_reports_no_divergence_on_equal_results\(\)/);
  assert.match(runtime, /fn put_blob_result_cache_entry\(&self, key: &str, entry: RuntimeResultCacheEntry\)/);
  assert.match(runtime, /result_cache_fingerprint\(&entry\.result\)\.into_bytes\(\)/);
  assert.match(runtime, /result_blob_entries\.read\(\)/);

  assert.match(followUp, /Current adapter stores only a Blob Cache fingerprint/);
  assert.match(followUp, /Eligible result-cache entries can be served after runtime restart from Blob Cache L2/);
});

test("Blob Cache benchmark and API review status is repeatable or explicitly split", () => {
  const bench = read("crates/reddb-server/benches/blob_cache_bench.rs");
  const benchDoc = read("docs/perf/blob-cache-bench-2026-05-06.md");
  const apiReview = read("docs/blob-cache-api-review-2026-05-06.md");
  const followUp = read("issues/349-blob-cache-redis-baseline-completion.md");

  for (const workload of [
    "w1_hot_l1_hit",
    "w2_cold_l2_miss",
    "w3_cold_absent",
    "w4_large_blob_l2_hit",
    "w5_namespace_flush",
    "w6_dependency_invalidation",
    "w7_restart_warm_cache",
    "w8_mixed_blob_admission",
  ]) {
    assert.match(bench, new RegExp(`fn ${workload}\\(`));
  }
  assert.match(benchDoc, /Cited session id slot: `sess-2026-05-07-bench-1954`/);
  assert.match(benchDoc, /Redis 7\.4/);
  assert.match(benchDoc, /deferred/);
  assert.match(followUp, /Fill the remaining Redis and hit-rate cells/);

  assert.match(apiReview, /Public HTTP\/SQL exposure stays deferred/);
  assert.match(apiReview, /internal-only surface/);
});
