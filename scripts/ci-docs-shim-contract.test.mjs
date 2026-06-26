// Issue #1307 — a docs-only / `**.md`-only PR cannot satisfy the `main` merge
// gate. ci.yml is path-filtered (paths-ignore: **.md, docs/**, ...), so a
// docs-only PR skips ci.yml and none of the required status checks (ADR 0059)
// ever report -> branch protection treats them as pending -> the PR is BLOCKED
// forever, even to `gh pr merge --admin` (enforce_admins).
//
// ci-docs.yml is the always-green shim: a workflow with NO paths-ignore that
// re-reports every required context as success on docs-only PRs, so the gate is
// satisfiable without lifting protection. GitHub matches required checks by
// context NAME regardless of producing workflow, so a same-named success from
// the shim satisfies the requirement.
//
// This contract test is the lock that keeps three artifacts in agreement:
//   - .github/required-status-checks.json  (the canonical list)
//   - .github/workflows/ci.yml             (produces the contexts on code PRs)
//   - .github/workflows/ci-docs.yml        (re-produces them on docs PRs)
// If a required job is renamed/added in ci.yml without updating the manifest and
// the shim, this test fails on the (non-docs) PR that makes the change.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");
const read = (rel) => fs.readFileSync(path.join(repoRoot, rel), "utf8");

const MANIFEST = ".github/required-status-checks.json";
const CI = ".github/workflows/ci.yml";
const SHIM = ".github/workflows/ci-docs.yml";

// Job-level `name:` lines are indented exactly four spaces (jobs: 0, job-key: 2,
// name: 4). Step names sit deeper (>=6) and the workflow name at 0, so this
// regex isolates job display names == status-check contexts.
function jobNames(yaml) {
  return [...yaml.matchAll(/^ {4}name: (.+)$/gm)].map((m) => m[1].trim());
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

test("the shim triggers exactly on the paths ci.yml ignores (so it covers the gap)", () => {
  const shim = read(SHIM);
  // ci.yml's PR paths-ignore set; the shim must allow-list the same globs.
  for (const glob of ["**.md", "docs/**", "CHANGELOG**", "LICENSE**"]) {
    assert.ok(shim.includes(`- '${glob}'`), `ci-docs.yml does not trigger on ${glob}`);
  }
  // It must be a pull_request workflow with no paths-ignore (else it would be
  // skipped on the very docs-only PRs it exists to cover).
  assert.match(shim, /pull_request:/);
  assert.ok(!/^\s*paths-ignore:/m.test(shim), "ci-docs.yml must not use paths-ignore");
});
