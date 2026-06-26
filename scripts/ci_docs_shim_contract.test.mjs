import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

const CI = ".github/workflows/ci.yml";
const SHIM = ".github/workflows/ci-docs-shim.yml";

// Issue #1307 — these are the contexts main's branch protection requires
// (`repos/reddb-io/reddb/branches/main/protection/required_status_checks`,
// #975 / ADR 0059). They are produced by ci.yml, which is path-filtered with
// `paths-ignore: ['**.md', 'docs/**', 'CHANGELOG**', 'LICENSE**']`. On a
// docs-only PR ci.yml never runs, so none of these report and the PR is
// BLOCKED forever. The shim workflow reports them green on docs-only PRs.
//
// This list is the source of truth for the shim. If branch protection's
// required set changes, update BOTH this list and ci-docs-shim.yml together —
// that lockstep is exactly what this test guards.
const REQUIRED_CONTEXTS = [
  "gate",
  "Quality (fmt, check, clippy)",
  "Lint (no untyped serialization)",
  "Version integrity",
  "Contract Matrix Gate",
  "Docs Match Contract Matrix",
  "Helm Chart",
  "AFK Validation Sidecar",
  "RQL Conformance (sqllogictest)",
  "Drivers / Python (cargo check)",
  "Feature Matrix (all-features)",
  "Feature Matrix (backend-d1)",
  "Feature Matrix (backend-s3)",
  "Feature Matrix (backend-turso)",
  "Feature Matrix (no-default)",
  "Feature Matrix (otel)",
  "Driver Param Conformance",
  "Chaos & Drill Suite",
  "Fuzz Parsers",
  "Container Stack",
  "Publish Dry-Run (crates.io)",
  "cargo package dry-run",
  "Windows (build + unit tests)",
  "macOS (build + unit tests)",
  "Test Suite",
];

// The doc-path globs ci.yml ignores on pull_request. The shim must trigger on
// exactly these so the two workflows partition cleanly: code-only PR -> only
// ci.yml; docs-only PR -> only the shim.
const DOC_PATHS = ["**.md", "docs/**", "CHANGELOG**", "LICENSE**"];

// Extract the YAML block-list items nested under a `context:` key (the matrix
// dimension that names each emitted check). Items may be single-quoted.
function matrixContexts(yaml) {
  const lines = yaml.split("\n");
  const start = lines.findIndex((l) => /^\s*context:\s*$/.test(l));
  assert.notEqual(start, -1, "shim must declare a `context:` matrix dimension");
  const indent = lines[start].match(/^\s*/)[0].length;
  const out = [];
  for (let i = start + 1; i < lines.length; i++) {
    const m = lines[i].match(/^(\s*)-\s+(.*\S)\s*$/);
    if (!m || m[1].length <= indent) break;
    out.push(m[2].replace(/^['"]|['"]$/g, ""));
  }
  return out;
}

test("the docs-only shim workflow exists", () => {
  assert.ok(
    fs.existsSync(path.join(repoRoot, SHIM)),
    `${SHIM} must exist so docs-only PRs can satisfy the merge gate`,
  );
});

test("shim triggers on pull_request for the exact doc paths ci.yml ignores", () => {
  const shim = read(SHIM);
  // Must be a pull_request workflow with a positive `paths:` filter (not
  // paths-ignore) so it fires precisely when ci.yml is path-skipped.
  assert.match(shim, /pull_request:/, "shim must run on pull_request");
  assert.match(shim, /^\s*paths:\s*$/m, "shim must use a positive `paths:` filter");
  for (const glob of DOC_PATHS) {
    assert.ok(
      shim.includes(`'${glob}'`) || shim.includes(`"${glob}"`) || new RegExp(`-\\s*${glob.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*$`, "m").test(shim),
      `shim paths must include ${glob} (lockstep with ci.yml paths-ignore)`,
    );
  }
});

test("ci.yml still path-ignores the doc paths on pull_request", () => {
  const ci = read(CI);
  assert.match(ci, /paths-ignore:/, "ci.yml must keep its paths-ignore filter");
  for (const glob of DOC_PATHS) {
    assert.ok(
      ci.includes(`'${glob}'`) || ci.includes(`"${glob}"`),
      `ci.yml paths-ignore must include ${glob} (lockstep with the shim)`,
    );
  }
});

test("shim emits exactly the branch-protection required contexts", () => {
  const shim = read(SHIM);
  // `gate` is its own job; the remaining 24 come from the matrix.
  assert.match(shim, /name:\s*gate\b/, "shim must emit the `gate` context");
  const emitted = new Set(["gate", ...matrixContexts(shim)]);
  const required = new Set(REQUIRED_CONTEXTS);

  const missing = [...required].filter((c) => !emitted.has(c));
  const extra = [...emitted].filter((c) => !required.has(c));
  assert.deepEqual(missing, [], `shim is missing required contexts: ${missing.join(", ")}`);
  assert.deepEqual(extra, [], `shim emits non-required contexts: ${extra.join(", ")}`);
});

test("shim is a no-op (cheap) — no cargo/docker/build commands", () => {
  const shim = read(SHIM);
  // Inspect only `run:` command lines — context NAMES legitimately contain the
  // words "cargo"/"docker" (e.g. "cargo package dry-run"), but no step may
  // actually invoke them.
  const runCmds = shim
    .split("\n")
    .map((l) => l.match(/^\s*-?\s*run:\s*(.*)$/))
    .filter(Boolean)
    .map((m) => m[1]);
  for (const cmd of runCmds) {
    assert.doesNotMatch(cmd, /\b(cargo|docker|node|cmake|gradlew)\b/, `shim run step must be a no-op, got: ${cmd}`);
  }
});
