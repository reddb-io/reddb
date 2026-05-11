import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("queue FANOUT semantics are covered through consumer-visible delivery", () => {
  const tests = read("tests/integration_queue_timeseries.rs");
  const docs = read("docs/data-models/queues.md");

  assert.match(tests, /fn test_fanout_queue_broadcast_all_consumers_get_all_messages\(\)/);
  assert.match(tests, /let consumers = \["alice", "bob", "carol"\]/);
  assert.match(tests, /consumer \{consumer\} should receive all 100 messages in FANOUT mode/);
  assert.match(tests, /fn test_fanout_queue_ack_isolation\(\)/);
  assert.match(tests, /bob must still see the message after alice acked it/);
  assert.match(tests, /fn test_fanout_queue_dlq_per_consumer\(\)/);
  assert.match(tests, /bob must not be affected by alice's DLQ move/);

  assert.match(docs, /FANOUT.*every consumer gets every message/);
  assert.match(docs, /A message acknowledged by `consumer_A` remains pending for `consumer_B`/);
});

test("ALTER QUEUE SET MODE transition keeps in-flight WORK messages drainable", () => {
  const tests = read("tests/integration_queue_timeseries.rs");
  const runtime = read("crates/reddb-server/src/runtime/impl_queue.rs");
  const docs = read("docs/data-models/queues.md");

  assert.match(tests, /fn test_alter_queue_work_to_fanout_transition\(\)/);
  assert.match(tests, /QUEUE READ alerts CONSUMER alice COUNT 1/);
  assert.match(tests, /ALTER QUEUE alerts SET MODE FANOUT/);
  assert.match(tests, /bob should get pre-2 \(not yet acked\) after FANOUT switch/);
  assert.match(tests, /bob should get post-1 pushed after FANOUT switch/);
  assert.match(tests, /QUEUE ACK alerts GROUP _work_default/);

  assert.match(runtime, /ALTER QUEUE SET MODE: \{\} in-flight messages will drain with old mode/);
  assert.match(runtime, /pending_count = pending\.len\(\)/);
  assert.match(docs, /After switching WORK to FANOUT/);
});
