#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const manifestPath = path.resolve(repoRoot, "examples/driver-pocs/manifest.json");
const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
const matrixPath = path.resolve(repoRoot, manifest.matrix ?? "");
const matrix = JSON.parse(fs.readFileSync(matrixPath, "utf8"));
const violations = [];

const matrixById = new Map(matrix.drivers.map((driver) => [driver.id, driver]));
const pocs = Array.isArray(manifest.drivers) ? manifest.drivers : [];
const seen = new Set();

for (const poc of pocs) {
  if (seen.has(poc.id)) violations.push(`${poc.id}: duplicate PoC`);
  seen.add(poc.id);

  const driver = matrixById.get(poc.id);
  if (!driver) {
    violations.push(`${poc.id}: not an official facade in the deployment-mode matrix`);
    continue;
  }
  const cell = driver.modes?.[poc.mode];
  if (!cell) {
    violations.push(`${poc.id}: unknown deployment mode ${String(poc.mode)}`);
  } else if (cell.status === "missing") {
    violations.push(`${poc.id}: PoC claims missing mode ${poc.mode}`);
  }
  if (!poc.evidence || !fs.existsSync(path.resolve(repoRoot, poc.evidence))) {
    violations.push(`${poc.id}: evidence does not exist: ${poc.evidence ?? "<missing>"}`);
  }
  if (typeof poc.command !== "string" || poc.command.trim() === "") {
    violations.push(`${poc.id}: command is required`);
  }
}

for (const id of matrixById.keys()) {
  if (!seen.has(id)) violations.push(`${id}: official facade has no PoC`);
}

if (pocs.length !== matrix.drivers.length) {
  violations.push(`expected ${matrix.drivers.length} PoCs, found ${pocs.length}`);
}

if (violations.length > 0) {
  console.error(`driver-pocs: ${violations.length} violation(s):`);
  for (const violation of violations) console.error(`  - ${violation}`);
  process.exit(1);
}

console.log(`driver-pocs: OK — ${pocs.length} official language facades have an executable journey.`);
