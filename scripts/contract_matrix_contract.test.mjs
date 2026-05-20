import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

const MATRIX = "docs/conformance/public-surface-contract-matrix.json";
const VERIFIER = "scripts/verify-contract-matrix.mjs";

test("contract matrix is machine-readable JSON with promises, surfaces, statuses", () => {
  const matrix = JSON.parse(read(MATRIX));
  assert.ok(Array.isArray(matrix.promises) && matrix.promises.length > 0);
  assert.ok(matrix.surfaces && Object.keys(matrix.surfaces).length > 0);
  // The surfaces named in the issue must all be columns.
  for (const surface of ["sql", "http", "redwire", "grpc", "driver_helpers"]) {
    assert.ok(surface in matrix.surfaces, `missing surface ${surface}`);
  }
  for (const promise of matrix.promises) {
    assert.match(promise.id, /^PSC-\d{3}$/);
    assert.ok(typeof promise.promise === "string" && promise.promise.length > 0);
    assert.ok(typeof promise.source === "string" && promise.source.length > 0);
    for (const surface of Object.keys(matrix.surfaces)) {
      assert.ok(promise.cells[surface], `${promise.id} missing cell ${surface}`);
      assert.ok(
        ["supported", "partial", "unsupported"].includes(promise.cells[surface].status),
      );
    }
  }
});

test("every supported/partial cell names an existing test", () => {
  const matrix = JSON.parse(read(MATRIX));
  for (const promise of matrix.promises) {
    for (const [surface, cell] of Object.entries(promise.cells)) {
      if (cell.status === "supported" || cell.status === "partial") {
        assert.ok(
          Array.isArray(cell.tests) && cell.tests.length > 0,
          `${promise.id}.${surface}: ${cell.status} but no tests`,
        );
        for (const t of cell.tests) {
          assert.ok(
            fs.existsSync(path.join(repoRoot, t)),
            `${promise.id}.${surface}: missing test ${t}`,
          );
        }
      }
    }
  }
});

test("verifier exits 0 on the checked-in matrix", () => {
  const out = execFileSync("node", [VERIFIER], { cwd: repoRoot, encoding: "utf8" });
  assert.match(out, /every supported\/partial cell has a backing test/);
});

test("verifier exits non-zero on a supported cell with no/dangling test", () => {
  const matrix = JSON.parse(read(MATRIX));
  matrix.promises[0].cells.sql = { status: "supported", tests: ["tests/__no_such_file__.rs"] };
  const tmp = path.join(os.tmpdir(), `contract-matrix-${process.pid}.json`);
  fs.writeFileSync(tmp, JSON.stringify(matrix));
  try {
    assert.throws(
      () => execFileSync("node", [VERIFIER, tmp], { cwd: repoRoot, stdio: "pipe" }),
      /does not exist on disk/,
    );
  } finally {
    fs.rmSync(tmp, { force: true });
  }
});

test("contract matrix gate is wired into CI and the release workflow", () => {
  const ci = read(".github/workflows/ci.yml");
  const release = read(".github/workflows/release.yml");
  assert.match(ci, /node scripts\/verify-contract-matrix\.mjs/);
  assert.match(ci, /contract-matrix:/);
  assert.match(release, /node scripts\/verify-contract-matrix\.mjs/);
  // Release gate must live in the `plan` job that every publish-* job needs.
  assert.match(release, /release-blocking/i);
});

test("release-policy ownership is recorded for human sign-off (HITL)", () => {
  const codeowners = read(".github/CODEOWNERS");
  assert.match(codeowners, /docs\/conformance\//);
  assert.match(codeowners, /verify-contract-matrix\.mjs/);
});

test("docs explain how to add and remove a promised feature", () => {
  const doc = read("docs/conformance/public-surface-contract-matrix.md");
  assert.match(doc, /## Adding a promised feature/i);
  assert.match(doc, /## Removing a promised feature/i);
  assert.match(doc, /verify-contract-matrix\.mjs/);
  assert.match(doc, /public-surface-contract-matrix\.json/);
});
