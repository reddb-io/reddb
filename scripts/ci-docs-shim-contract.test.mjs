// Keep the automatic CI and docs-shim context names aligned with ADR 0059.
// The core correctness lane and quick-checks.yml run automatically; broader
// release-adjacent validation remains opt-in.
// Check the merge-group classifier and failure-propagating aggregate separately
// so a future trigger change cannot silently bypass the guarded heavy jobs.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");
const read = (rel) => fs.readFileSync(path.join(repoRoot, rel), "utf8").replace(/\r\n/g, "\n");

const MANIFEST = ".github/required-status-checks.json";
const CI = ".github/workflows/ci.yml";
const SHIM = ".github/workflows/ci-docs.yml";

// Job-level `name:` lines are indented exactly four spaces (jobs: 0, job-key: 2,
// name: 4). Step names sit deeper (>=6) and the workflow name at 0, so this
// regex isolates job display names == status-check contexts.
function jobNames(yaml) {
  return [...yaml.matchAll(/^ {4}name: (.+)$/gm)].map((m) => m[1].trim());
}

function jobBlocks(yaml) {
  const jobs = yaml.slice(yaml.indexOf("\njobs:\n") + "\njobs:\n".length);
  const starts = [...jobs.matchAll(/^ {2}([a-zA-Z0-9_-]+):$/gm)];
  return new Map(starts.map((match, index) => {
    const end = starts[index + 1]?.index ?? jobs.length;
    return [match[1], jobs.slice(match.index, end)];
  }));
}

const manifest = JSON.parse(read(MANIFEST));
const required = manifest.required_contexts;

test("manifest is a well-formed, unique, non-empty list of context strings", () => {
  assert.ok(Array.isArray(required) && required.length > 0);
  for (const ctx of required) {
    assert.equal(typeof ctx, "string");
    assert.ok(ctx.length > 0);
  }
  assert.equal(new Set(required).size, required.length, "duplicate contexts in manifest");
});

test("manifest matches the 25 required contexts recorded in ADR 0059", () => {
  // ADR 0059 fixes the gate at 25 contexts. Locking the count makes any silent
  // expansion/contraction of the manifest a test failure, not a surprise.
  assert.equal(required.length, 25);
});

test("the docs shim re-produces every required context (and nothing extra)", () => {
  const shimContexts = new Set(jobNames(read(SHIM)));
  for (const ctx of required) {
    assert.ok(shimContexts.has(ctx), `ci-docs.yml is missing required context: ${ctx}`);
  }
  // The shim must not claim contexts outside the required set — a stray green
  // job named like a real one could mask a genuine failure on a mixed PR.
  for (const ctx of shimContexts) {
    assert.ok(required.includes(ctx), `ci-docs.yml emits a non-required context: ${ctx}`);
  }
});

test("every required context is actually produced by ci.yml (manifest reflects reality)", () => {
  const ci = read(CI);
  const ciNames = jobNames(ci);
  const featureLabels = ["no-default", "otel", "backend-s3", "backend-turso", "backend-d1", "all-features"];

  for (const ctx of required) {
    const fm = ctx.match(/^Feature Matrix \((.+)\)$/);
    if (fm) {
      // ci.yml renders this context via the matrix template
      // `Feature Matrix (${{ matrix.features.label }})`.
      assert.ok(ciNames.includes("Feature Matrix (${{ matrix.features.label }})"),
        "ci.yml lost the Feature Matrix job");
      assert.ok(featureLabels.includes(fm[1]), `unknown Feature Matrix label: ${fm[1]}`);
      assert.match(ci, new RegExp(`label: ${fm[1]}\\b`),
        `ci.yml feature-matrix is missing label: ${fm[1]}`);
      continue;
    }
    assert.ok(ciNames.includes(ctx), `ci.yml no longer produces required context: ${ctx}`);
  }
});

test("core CI and the docs shim run automatically, with manual full-CI retained", () => {
  const ciTriggers = read(CI).match(/^on:\n([\s\S]*?)(?=^[^\s#])/m)?.[1];
  assert.ok(ciTriggers, `${CI} must define triggers`);
  for (const event of ["merge_group", "push", "pull_request", "workflow_dispatch"]) {
    assert.match(ciTriggers, new RegExp(`^  ${event}:`, "m"));
  }
  assert.doesNotMatch(ciTriggers, /^  schedule:/m);

  const shimTriggers = read(SHIM).match(/^on:\n([\s\S]*?)(?=^[^\s#])/m)?.[1];
  assert.ok(shimTriggers, `${SHIM} must define triggers`);
  for (const event of ["pull_request", "workflow_dispatch"]) {
    assert.match(shimTriggers, new RegExp(`^  ${event}:`, "m"));
  }
  assert.doesNotMatch(shimTriggers, /^  (push|merge_group|schedule):/m);

  const quick = read(".github/workflows/quick-checks.yml");
  for (const event of ["pull_request", "push", "merge_group"]) {
    assert.match(quick, new RegExp(`^  ${event}:`, "m"));
  }
});

test("platform selection precedes the gate and the gate classifies merge-group diffs", () => {
  const ci = read(CI);
  const jobs = jobBlocks(ci);
  assert.deepEqual([...jobs.keys()].slice(0, 2), ["platforms", "gate"]);
  const platforms = jobs.get("platforms");
  assert.match(platforms, /^    timeout-minutes: 3$/m);
  assert.match(platforms, /^    permissions: \{\}$/m);
  assert.doesNotMatch(platforms, /actions\/checkout/);

  const gate = jobs.get("gate");
  assert.match(gate, /needs:\n\s+- platforms/);
  assert.match(gate, /run_heavy: \$\{\{ steps\.classify\.outputs\.run_heavy \}\}/);
  assert.match(gate, /fetch-depth: 2/, "the fast classifier should fetch only the merge commit and its parent");
  assert.match(gate, /BASE_SHA: \$\{\{ github\.event\.merge_group\.base_sha \}\}/);
  assert.match(gate, /HEAD_SHA: \$\{\{ github\.event\.merge_group\.head_sha \}\}/);
  assert.match(gate, /git diff --name-only "\$BASE_SHA" "\$HEAD_SHA"/);
  assert.match(gate, /run_heavy=true/);
  assert.match(gate, /! grep -Eqv "\$DOCS_ONLY_PATTERN" "\$changed_files"; then\n {14}run_heavy=false/);

  const pattern = gate.match(/DOCS_ONLY_PATTERN: '([^']+)'/)?.[1];
  assert.ok(pattern, "gate must declare its docs-only path pattern");
  const docsOnly = ["README.md", "guides/install.md", "docs/index.html", "CHANGELOG", "CHANGELOG.md", "LICENSE", "LICENSE-APACHE"];
  const codeTouching = ["src/lib.rs", "Cargo.toml", "scripts/check.sh", "nested/CHANGELOG", "nested/LICENSE"];
  for (const file of docsOnly) assert.match(file, new RegExp(pattern));
  for (const file of codeTouching) assert.doesNotMatch(file, new RegExp(pattern));
});

test("docs-only merge groups skip every heavy job and code diffs keep them enabled", () => {
  const jobs = jobBlocks(read(CI));
  jobs.delete("gate");
  jobs.delete("platforms");
  assert.ok(jobs.size > 0);
  for (const [jobId, block] of jobs) {
    assert.ok(block.includes("needs.gate.outputs.run_heavy == 'true'"),
      `${jobId} is not guarded by the merge-group classifier`);
  }
});

test("Test Suite shards on free runners and reports one failure-propagating required context", () => {
  const jobs = jobBlocks(read(CI));
  const shards = jobs.get("test-shards");
  const aggregate = jobs.get("tests");

  assert.ok(shards, "ci.yml must define the test-shards matrix job");
  assert.ok(shards.includes(
    "name: Test Suite ${{ matrix.shard == 'unit' && 'unit' || format('shard ({0}/4)', matrix.shard) }}",
  ));
  assert.match(shards, /^ {4}runs-on: ubuntu-24\.04$/m,
    "test shards must use free GitHub-hosted public runners");
  assert.match(shards, /shard: \[unit, 1, 2, 3, 4\]/);
  assert.match(shards, /name: Run non-integration test layer\n\s+if: matrix\.shard == 'unit'/);
  assert.match(shards, /name: Run integration-test shard\n\s+if: matrix\.shard != 'unit'/);
  assert.match(shards, /scripts\/nextest-e2e-shard\.sh "\$\{\{ matrix\.shard \}\}" 4/);

  assert.ok(aggregate, "ci.yml must preserve a Test Suite aggregate job");
  assert.match(aggregate, /^ {4}name: Test Suite$/m);
  assert.match(aggregate, /^ {4}runs-on: ubuntu-latest$/m,
    "the required-context aggregate must use a free GitHub-hosted public runner");
  assert.match(aggregate, /needs:\n\s+- gate\n\s+- test-shards\n\s+- client-size-budget\n\s+- platforms/);
  assert.match(aggregate, /always\(\).*needs\.gate\.outputs\.run_heavy == 'true'/);
  assert.match(aggregate, /needs\.test-shards\.result.*success/,
    "the required Test Suite context must fail when any shard fails");
  assert.match(aggregate, /needs\.client-size-budget\.result.*success/,
    "the required Test Suite context must fail when the client-size gate fails");

  const clientSize = jobs.get("client-size-budget");
  assert.ok(clientSize, "the release-size build must have a separate runner");
  assert.match(clientSize, /run: \.\/scripts\/check-red-client-size\.sh/);
  assert.doesNotMatch(shards, /check-red-client-size\.sh/,
    "release artifacts must not fill the integration runner's disk");
  const cacheKey = (block) => block.match(/shared-key: (.+)/)?.[1];
  assert.ok(cacheKey(clientSize));
  assert.ok(cacheKey(shards));
  assert.notEqual(cacheKey(clientSize), cacheKey(shards),
    "test shards must not restore a cache containing the release-size build");
});
