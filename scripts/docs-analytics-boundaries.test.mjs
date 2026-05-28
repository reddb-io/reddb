import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("Analytics v0 docs keep raw writes on ordinary collections", () => {
  const analytics = read("docs/data-models/analytics.md");
  const metrics = read("docs/data-models/metrics.md");

  for (const doc of [analytics, metrics]) {
    assert.match(doc, /CREATE TABLE product_events/);
    assert.match(doc, /INSERT INTO product_events/);
    assert.match(doc, /CREATE ANALYTICS SOURCE product_events/);
    assert.doesNotMatch(doc, /INSERT INTO METRIC/i);
  }
});

test("Analytics v0 source and metric descriptors are catalog records, not adapters", () => {
  const analytics = read("docs/data-models/analytics.md");
  const metrics = read("docs/data-models/metrics.md");
  const combined = `${analytics}\n${metrics}`;

  assert.match(analytics, /red\.analytics\.sources/);
  assert.match(
    metrics,
    /Prometheus compatibility is a Metrics collection and wire surface, not the\s+Analytics v0 catalog/,
  );
  assert.doesNotMatch(combined, /source collection, query, adapter/i);
  assert.doesNotMatch(combined, /generic analytics object/i);
});

test("Time-Series and probabilistic docs stay below the Analytics product API", () => {
  const timeseries = read("docs/data-models/timeseries.md");
  const probabilistic = read("docs/data-models/probabilistic.md");

  assert.match(timeseries, /does not own product or reliability semantics/i);
  assert.match(timeseries, /metric descriptors may read from or materialize into Time-Series/i);
  assert.match(probabilistic, /execution primitives and sidecars/i);
  assert.match(probabilistic, /not the Analytics product API/i);
});

test("overview routes Analytics v0 to metric descriptors instead of adapter promises", () => {
  const overview = read("docs/data-models/overview.md");

  assert.match(overview, /metric descriptors/i);
  assert.doesNotMatch(overview, /\| Metrics \| `CREATE METRICS` \(planned\), Prometheus `remote_write`/);
});
