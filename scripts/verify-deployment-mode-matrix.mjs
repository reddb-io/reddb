#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const matrixPath = path.resolve(
  process.argv[2] ?? "docs/conformance/deployment-mode-matrix.json",
);
const requiredModes = ["serverless", "embedded", "server_replica", "cluster"];
const validStatuses = new Set(["qualified", "partial", "missing"]);

function fail(code, message) {
  console.error(`deployment-mode-matrix: ${message}`);
  process.exit(code);
}

let matrix;
try {
  matrix = JSON.parse(fs.readFileSync(matrixPath, "utf8"));
} catch (error) {
  fail(2, `cannot read/parse ${path.relative(repoRoot, matrixPath)}: ${error.message}`);
}

if (!matrix || typeof matrix !== "object" || !Array.isArray(matrix.drivers)) {
  fail(2, "matrix is missing a `drivers` array");
}

const violations = [];
const declaredModes = matrix.modes && typeof matrix.modes === "object"
  ? Object.keys(matrix.modes)
  : [];
if (
  declaredModes.length !== requiredModes.length ||
  requiredModes.some((mode) => !declaredModes.includes(mode))
) {
  violations.push(`the canonical modes must be exactly: ${requiredModes.join(", ")}`);
}

const contract = path.resolve(repoRoot, matrix.contract ?? "");
if (!matrix.contract || !fs.existsSync(contract)) {
  violations.push(`contract does not exist: ${matrix.contract ?? "<missing>"}`);
}

const seenIds = new Set();
const counts = { qualified: 0, partial: 0, missing: 0 };
let testReferences = 0;
let readyDrivers = 0;

for (const driver of matrix.drivers) {
  const id = driver?.id ?? "<missing-id>";
  if (seenIds.has(id)) violations.push(`${id}: duplicate driver id`);
  seenIds.add(id);

  if (!driver.language || !driver.facade) {
    violations.push(`${id}: language and facade are required`);
  }
  const roots = Array.isArray(driver.package_roots) ? driver.package_roots : [];
  if (roots.length === 0) violations.push(`${id}: package_roots must not be empty`);
  for (const root of roots) {
    if (!fs.existsSync(path.resolve(repoRoot, root))) {
      violations.push(`${id}: package root does not exist: ${root}`);
    }
  }

  const cells = driver.modes && typeof driver.modes === "object" ? driver.modes : {};
  const cellModes = Object.keys(cells);
  for (const mode of requiredModes) {
    if (!(mode in cells)) violations.push(`${id}: missing mode cell ${mode}`);
  }
  for (const mode of cellModes) {
    if (!requiredModes.includes(mode)) violations.push(`${id}: unknown mode cell ${mode}`);
  }

  let allQualified = true;
  for (const mode of requiredModes) {
    const cell = cells[mode];
    if (!cell) {
      allQualified = false;
      continue;
    }
    const status = cell.status;
    if (!validStatuses.has(status)) {
      violations.push(`${id}.${mode}: invalid status ${String(status)}`);
      allQualified = false;
      continue;
    }
    counts[status]++;
    if (status !== "qualified") allQualified = false;

    const tests = Array.isArray(cell.tests) ? cell.tests : [];
    const gap = typeof cell.gap === "string" ? cell.gap.trim() : "";
    if (status === "qualified" || status === "partial") {
      if (tests.length === 0) {
        violations.push(`${id}.${mode}: ${status} requires automated test evidence`);
      }
      for (const test of tests) {
        if (!fs.existsSync(path.resolve(repoRoot, test))) {
          violations.push(`${id}.${mode}: test evidence does not exist: ${test}`);
        } else {
          testReferences++;
        }
      }
    } else if (tests.length > 0) {
      violations.push(`${id}.${mode}: missing support must not list passing tests`);
    }
    if (status === "partial" && !gap) {
      violations.push(`${id}.${mode}: partial support requires a concrete gap`);
    }
    if (status === "missing" && !gap) {
      violations.push(`${id}.${mode}: missing support requires a concrete gap`);
    }
    if (status === "qualified" && gap) {
      violations.push(`${id}.${mode}: qualified support must not retain a gap`);
    }
  }

  if (driver.four_mode_ready !== allQualified) {
    violations.push(
      `${id}: four_mode_ready=${String(driver.four_mode_ready)} but all four modes qualified=${String(allQualified)}`,
    );
  }
  if (allQualified) readyDrivers++;
}

const cells = counts.qualified + counts.partial + counts.missing;
console.log(
  `deployment-mode-matrix: ${matrix.drivers.length} official facades, ${cells} cells ` +
    `(${counts.qualified} qualified, ${counts.partial} partial, ${counts.missing} missing), ` +
    `${readyDrivers} four-mode ready, ${testReferences} test references verified.`,
);

if (violations.length > 0) {
  console.error(`\ndeployment-mode-matrix: ${violations.length} violation(s) — release blocked:`);
  for (const violation of violations) console.error(`  - ${violation}`);
  process.exit(1);
}

console.log("deployment-mode-matrix: OK — every official facade accounts for all four modes.");
