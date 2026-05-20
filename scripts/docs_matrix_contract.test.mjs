import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

import {
  applyBlock,
  buildTargets,
  extractBlock,
  loadMatrix,
  renderBlock,
  resolveSource,
} from "./gen-docs-from-matrix.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const GEN = "scripts/gen-docs-from-matrix.mjs";
const MATRIX = "docs/conformance/public-surface-contract-matrix.json";

function read(rel) {
  return fs.readFileSync(path.join(repoRoot, rel), "utf8");
}

const matrix = loadMatrix(path.join(repoRoot, MATRIX));

test("doc-check tooling exists and runs", () => {
  assert.ok(fs.existsSync(path.join(repoRoot, GEN)));
});

test("checked-in docs are in sync with the matrix (gate passes)", () => {
  const out = execFileSync("node", [GEN, "--check"], { cwd: repoRoot, encoding: "utf8" });
  assert.match(out, /every public doc matches the contract matrix/);
});

test("README, query docs, and driver READMEs are all covered targets", () => {
  const files = buildTargets(matrix).map((t) => t.file);
  assert.ok(files.includes("README.md"), "README.md covered");
  assert.ok(
    files.some((f) => f.startsWith("docs/query/")),
    "a docs/query/*.md doc covered",
  );
  assert.ok(
    files.includes("crates/reddb-client/README.md"),
    "rust client README covered",
  );
  assert.ok(
    files.filter((f) => f.startsWith("drivers/") && f.endsWith("/README.md")).length >= 5,
    "driver READMEs covered",
  );
});

test("every covered doc carries the generated block on disk", () => {
  for (const { file } of buildTargets(matrix)) {
    assert.ok(extractBlock(read(file)) !== null, `${file} is missing the contract-matrix block`);
  }
});

test("a generated block can never claim more than the matrix", () => {
  // The block is rendered purely from the matrix, so a doc cannot show a
  // status the matrix does not back. An `unsupported` cell must render as
  // "unsupported" in its promise row — never "supported"/"partial".
  const surfaces = Object.keys(matrix.surfaces);
  const block = renderBlock(matrix, { surfaces, promises: matrix.promises, intro: "x" });
  const rowFor = (id) => block.split("\n").find((l) => l.includes(`**${id}**`));
  for (const p of matrix.promises) {
    const row = rowFor(p.id);
    assert.ok(row, `${p.id} row present`);
    const cells = row.split("|").slice(2, 2 + surfaces.length).map((c) => c.trim());
    surfaces.forEach((surface, i) => {
      const status = p.cells[surface].status;
      if (status === "unsupported") {
        assert.match(cells[i], /unsupported/, `${p.id}.${surface} must render as unsupported`);
        assert.doesNotMatch(cells[i], /^✅ supported|partial/);
      }
    });
  }
});

test("--check fails (drift) when a doc upgrades a status by hand", () => {
  // Simulate a human editing a doc to claim a feature the matrix marks
  // unsupported: tamper the README block, then assert the regenerated content
  // differs (which is exactly what --check compares).
  const readme = read("README.md");
  const inner = buildTargets(matrix).find((t) => t.file === "README.md").inner;
  const expected = applyBlock(readme, inner);
  assert.equal(expected, readme, "checked-in README already matches generated output");

  const tampered = readme.replace("❌ unsupported", "✅ supported");
  assert.notEqual(tampered, readme, "test fixture: README has an unsupported cell to tamper");
  // Re-applying the matrix-derived block over the tampered doc restores truth,
  // i.e. the tampered doc is detected as out of date.
  assert.notEqual(applyBlock(tampered, inner), tampered);
});

test("--write is idempotent (running it again is a no-op)", () => {
  for (const { file, inner } of buildTargets(matrix)) {
    const current = read(file);
    assert.equal(applyBlock(current, inner), current, `${file} would change on re-write`);
  }
});

test("resolveSource maps matrix sources to existing repo files", () => {
  assert.equal(resolveSource("README.md"), "README.md");
  assert.equal(resolveSource("docs/query/insert.md"), "docs/query/insert.md");
  assert.equal(resolveSource("does/not/exist.md"), null);
});

test("doc-matrix check is wired into CI", () => {
  const ci = read(".github/workflows/ci.yml");
  assert.match(ci, /gen-docs-from-matrix\.mjs --check/);
  assert.match(ci, /docs-matrix:/);
});

test("docs explain doc generation/check against the matrix", () => {
  const doc = read("docs/conformance/public-surface-contract-matrix.md");
  assert.match(doc, /gen-docs-from-matrix\.mjs/);
  assert.match(doc, /## Generating and checking docs against the matrix/i);
});
