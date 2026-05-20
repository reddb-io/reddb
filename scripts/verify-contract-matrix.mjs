#!/usr/bin/env node
// Release-blocking gate for the public-surface contract matrix.
//
// Rule (issue #567): every cell marked `supported` or `partial` MUST name at
// least one automated test, and every named test must exist on disk. A
// `supported`/`partial` cell with no backing test — or a dangling test path —
// is a release blocker, so this script exits non-zero and the release/CI job
// fails. `unsupported` cells require nothing.
//
// Usage: node scripts/verify-contract-matrix.mjs [path-to-matrix.json]
// Exit codes: 0 = all enforced cells backed, 1 = violations, 2 = malformed input.

import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const matrixPath = path.resolve(
  process.argv[2] ?? "docs/conformance/public-surface-contract-matrix.json",
);

const ENFORCED = new Set(["supported", "partial"]);
const VALID_STATUS = new Set(["supported", "partial", "unsupported"]);

function fail(code, message) {
  console.error(`contract-matrix: ${message}`);
  process.exit(code);
}

let matrix;
try {
  matrix = JSON.parse(fs.readFileSync(matrixPath, "utf8"));
} catch (err) {
  fail(2, `cannot read/parse ${path.relative(repoRoot, matrixPath)}: ${err.message}`);
}

if (!matrix || typeof matrix !== "object" || !Array.isArray(matrix.promises)) {
  fail(2, "matrix is missing a `promises` array");
}
const surfaces = matrix.surfaces && typeof matrix.surfaces === "object"
  ? Object.keys(matrix.surfaces)
  : [];
if (surfaces.length === 0) {
  fail(2, "matrix is missing a non-empty `surfaces` map");
}

const violations = [];
const seenIds = new Set();
let supported = 0;
let partial = 0;
let unsupported = 0;
let backedTestRefs = 0;

for (const promise of matrix.promises) {
  const id = promise?.id ?? "<no-id>";
  if (seenIds.has(id)) violations.push(`${id}: duplicate promise id`);
  seenIds.add(id);
  if (!promise.cells || typeof promise.cells !== "object") {
    violations.push(`${id}: missing \`cells\``);
    continue;
  }
  // Every declared surface must have a cell, so a new surface can't be
  // silently left blank for an existing promise.
  for (const surface of surfaces) {
    if (!(surface in promise.cells)) {
      violations.push(`${id}: no cell for surface \`${surface}\``);
    }
  }
  for (const [surface, cell] of Object.entries(promise.cells)) {
    if (!surfaces.includes(surface)) {
      violations.push(`${id}.${surface}: unknown surface (not declared in \`surfaces\`)`);
    }
    const status = cell?.status;
    if (!VALID_STATUS.has(status)) {
      violations.push(`${id}.${surface}: invalid status \`${status}\``);
      continue;
    }
    if (status === "supported") supported++;
    else if (status === "partial") partial++;
    else unsupported++;

    const tests = Array.isArray(cell.tests) ? cell.tests : [];
    if (ENFORCED.has(status)) {
      if (tests.length === 0) {
        violations.push(
          `${id}.${surface}: status \`${status}\` but no automated test reference`,
        );
        continue;
      }
      for (const t of tests) {
        const abs = path.resolve(repoRoot, t);
        if (!fs.existsSync(abs)) {
          violations.push(`${id}.${surface}: test reference does not exist on disk: ${t}`);
        } else {
          backedTestRefs++;
        }
      }
    } else if (tests.length > 0) {
      // Tests on an `unsupported` cell are confusing — flag so the row is fixed.
      violations.push(
        `${id}.${surface}: status \`unsupported\` should not list tests (found ${tests.length})`,
      );
    }
  }
}

const cells = supported + partial + unsupported;
console.log(
  `contract-matrix: ${matrix.promises.length} promises, ${cells} cells ` +
    `(${supported} supported, ${partial} partial, ${unsupported} unsupported), ` +
    `${backedTestRefs} backing test refs verified on disk.`,
);

if (violations.length > 0) {
  console.error(`\ncontract-matrix: ${violations.length} violation(s) — release blocked:`);
  for (const v of violations) console.error(`  - ${v}`);
  console.error(
    "\nFix: back the cell with an existing test, downgrade it to `unsupported`,\n" +
      "or remove the promise per docs/conformance/public-surface-contract-matrix.md.",
  );
  process.exit(1);
}

console.log("contract-matrix: OK — every supported/partial cell has a backing test.");
