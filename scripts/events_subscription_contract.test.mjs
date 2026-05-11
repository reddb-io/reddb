import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("event subscriptions have public multi-target, fanout, and drop evidence", () => {
  const tests = read("tests/e2e_events_foundation.rs");
  const docs = read("docs/data-models/events.md");

  assert.match(tests, /fn add_two_subscriptions_both_receive_insert_event\(\)/);
  assert.match(tests, /ALTER TABLE orders ADD SUBSCRIPTION s1 TO q1/);
  assert.match(tests, /ALTER TABLE orders ADD SUBSCRIPTION s2 TO q2/);
  assert.match(tests, /read_event_payload\(&rt, "q1"\)/);
  assert.match(tests, /read_event_payload\(&rt, "q2"\)/);
  assert.match(tests, /queue\.q1\.mode/);
  assert.match(tests, /queue\.q2\.mode/);
  assert.match(tests, /contains\("fanout"\)/);

  assert.match(tests, /fn drop_subscription_stops_events_to_that_queue\(\)/);
  assert.match(tests, /ALTER TABLE events3 DROP SUBSCRIPTION s1/);
  assert.match(tests, /e3_q1 should be empty after s1 was dropped/);
  assert.match(tests, /assert_eq!\(contract\.subscriptions\[0\]\.name, "s2"\)/);

  assert.match(docs, /A collection can have multiple named subscriptions/);
  assert.match(docs, /Each subscription has its own target queue, filters, and redaction list/);
});

test("event subscriptions have per-subscription redaction evidence", () => {
  const tests = read("tests/e2e_events_foundation.rs");
  const docs = read("docs/data-models/events.md");

  assert.match(tests, /fn redact_applied_per_subscription_independently\(\)/);
  assert.match(tests, /ADD SUBSCRIPTION masked TO q_masked REDACT \(email\)/);
  assert.match(tests, /ADD SUBSCRIPTION unredacted TO q_unredacted/);
  assert.match(tests, /masked\["after"\]\["email"\]/);
  assert.match(tests, /unredacted\["after"\]\["email"\]/);

  assert.match(docs, /ADD SUBSCRIPTION masked TO pii_events REDACT \(customer_email\)/);
});
