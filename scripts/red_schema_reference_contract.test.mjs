import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("performance wins documentation is tied to reproducible benchmark evidence", () => {
  const wins = read("docs/perf/wins.md");
  const whenNot = read("docs/perf/when-not-reddb.md");
  const readme = read("README.md");
  const jsGuide = read("docs/guides/javascript-typescript-driver.md");

  assert.match(wins, /BenchConfigSchema/);
  assert.match(wins, /OFFICIAL_PROFILE =\s+standard/);
  assert.match(wins, /rdb-benchmark\/benchmarks\/history\.jsonl/);
  assert.match(wins, /make duel-official OFFICIAL_SCENARIOS=typed_insert/);
  assert.match(wins, /make duel-official OFFICIAL_SCENARIOS=disk_usage/);
  assert.match(wins, /sess-\d{14}-\d+/);
  assert.match(wins, /typed_insert/);
  assert.match(wins, /disk_usage/);

  assert.match(whenNot, /make duel-official/);
  assert.match(whenNot, /#157/);
  assert.match(whenNot, /#159/);
  assert.match(whenNot, /#161/);
  assert.match(readme, /docs\/perf\/wins\.md/);
  assert.match(jsGuide, /docs\/perf\/wins\.md/);
});

test("red schema reference is aligned with public introspection coverage", () => {
  const reference = read("docs/reference/red-schema.md");
  const docsIndex = read("docs/README.md");
  const e2e = read("tests/e2e_red_schema.rs");
  const conformance = read("crates/reddb-server/tests/conformance/show_collections.toml");

  for (const relation of [
    "red.collections",
    "red.columns",
    "red.indices",
    "red.policies",
    "red.stats",
    "red.subscriptions",
  ]) {
    assert.match(reference, new RegExp(`## \`${relation}\``));
  }

  for (const command of [
    "SHOW COLLECTIONS",
    "SHOW SCHEMA <collection>",
    "SHOW INDICES",
    "SHOW POLICIES",
    "SHOW STATS",
    "EVENTS STATUS",
  ]) {
    assert.match(reference, new RegExp(command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  }

  assert.match(reference, /All documented columns are stable since RedDB 0\.1/);
  assert.match(reference, /ADR 0010/);
  assert.match(reference, /ADR 0011/);
  assert.match(reference, /system schema is read-only/);
  assert.match(docsIndex, /reference\/red-schema\.md/);
  assert.match(e2e, /red_schema_introspection_is_stable_across_virtual_tables/);
  assert.match(e2e, /show_commands_match_red_schema_queries_for_stable_introspection/);
  assert.match(e2e, /red_schema_dml_is_read_only/);
  assert.match(conformance, /docs\/reference\/red-schema\.md/);
});
