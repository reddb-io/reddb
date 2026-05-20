#!/usr/bin/env node
// Doc generation / doc-check against the public-surface contract matrix.
//
// Issue #568: public docs (README, docs/query/*.md, driver READMEs) are
// generated from — and checked against — the contract matrix
// (docs/conformance/public-surface-contract-matrix.json). The matrix is the
// single source of truth (its release gate lives in
// scripts/verify-contract-matrix.mjs). This tool projects the matrix into a
// marker-delimited "Public-surface support" block inside each public doc, so
// a doc can never present a surface as more-supported than the matrix says:
// the block content is computed purely from the matrix, and `--check` fails
// CI on any drift.
//
// Usage:
//   node scripts/gen-docs-from-matrix.mjs --check   # CI gate: fail on drift / missing block
//   node scripts/gen-docs-from-matrix.mjs --write    # regenerate blocks in place
//   node scripts/gen-docs-from-matrix.mjs            # alias for --check
//
// Exit codes: 0 = in sync (or written), 1 = drift/missing block, 2 = malformed input.

import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const MATRIX_REL = "docs/conformance/public-surface-contract-matrix.json";

const BEGIN = "<!-- contract-matrix:begin -->";
const END = "<!-- contract-matrix:end -->";

const STATUS_LABEL = {
  supported: "✅ supported",
  partial: "⚠️ partial",
  unsupported: "❌ unsupported",
};

export function loadMatrix(absPath) {
  const matrix = JSON.parse(fs.readFileSync(absPath, "utf8"));
  if (!matrix || !Array.isArray(matrix.promises) || !matrix.surfaces) {
    throw new Error("matrix missing `promises` array or `surfaces` map");
  }
  return matrix;
}

// The matrix `source` strings are informal pointers: some are repo-root
// relative ("docs/query/insert.md"), some relative to docs/conformance/
// ("../feedbacks.md"). Resolve to a repo-root-relative path if the file
// exists, else null.
export function resolveSource(source) {
  const candidates = [
    source,
    path.posix.normalize(path.posix.join("docs/conformance", source)),
  ];
  for (const c of candidates) {
    const abs = path.resolve(repoRoot, c);
    if (abs.startsWith(repoRoot) && fs.existsSync(abs) && fs.statSync(abs).isFile()) {
      return path.relative(repoRoot, abs).split(path.sep).join("/");
    }
  }
  return null;
}

function shortPromise(p) {
  // Keep promise text on one table line.
  return p.promise.replace(/\s+/g, " ").trim();
}

// Render the inner content of a managed block (between the markers, exclusive).
export function renderBlock(matrix, { surfaces, promises, intro }) {
  const cols = surfaces;
  const header = `| Promise | ${cols.join(" | ")} |`;
  const sep = `|${" --- |".repeat(cols.length + 1)}`;
  const rows = promises.map((p) => {
    const cells = cols.map((s) => {
      const status = p.cells?.[s]?.status ?? "unsupported";
      return STATUS_LABEL[status] ?? status;
    });
    return `| **${p.id}** — ${shortPromise(p)} | ${cells.join(" | ")} |`;
  });
  return [
    "## Public-surface support",
    "",
    intro,
    "",
    header,
    sep,
    ...rows,
    "",
    "_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._",
  ].join("\n");
}

// Replace the managed region (markers + content) if present, else append it.
export function applyBlock(fileText, innerContent) {
  const managed = `${BEGIN}\n${innerContent}\n${END}`;
  const beginIdx = fileText.indexOf(BEGIN);
  const endIdx = fileText.indexOf(END);
  if (beginIdx !== -1 && endIdx !== -1 && endIdx > beginIdx) {
    const before = fileText.slice(0, beginIdx);
    const after = fileText.slice(endIdx + END.length);
    return before + managed + after;
  }
  const trimmed = fileText.replace(/\s*$/, "");
  return `${trimmed}\n\n${managed}\n`;
}

// Extract the current managed region (markers + content) or null.
export function extractBlock(fileText) {
  const beginIdx = fileText.indexOf(BEGIN);
  const endIdx = fileText.indexOf(END);
  if (beginIdx === -1 || endIdx === -1 || endIdx <= beginIdx) return null;
  return fileText.slice(beginIdx, endIdx + END.length);
}

const GEN_NOTE =
  "> Generated from " +
  "[`docs/conformance/public-surface-contract-matrix.json`](" +
  "/docs/conformance/public-surface-contract-matrix.json) by " +
  "`scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — " +
  "run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source " +
  "of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.";

const ALL_SURFACES_INTRO =
  GEN_NOTE +
  "\n>\n> Every public RedDB promise and the status of each public surface that offers it.";

// Build the full list of doc targets to manage.
export function buildTargets(matrix) {
  const surfaceKeys = Object.keys(matrix.surfaces);
  const targets = [];

  // 1. README.md — umbrella capability matrix across every surface.
  targets.push({
    file: "README.md",
    inner: renderBlock(matrix, {
      surfaces: surfaceKeys,
      promises: matrix.promises,
      intro: ALL_SURFACES_INTRO,
    }),
  });

  // 2. Each existing `source` doc — its own promises across every surface.
  const bySource = new Map();
  for (const p of matrix.promises) {
    const resolved = resolveSource(p.source);
    if (!resolved || resolved === "README.md") continue;
    if (!bySource.has(resolved)) bySource.set(resolved, []);
    bySource.get(resolved).push(p);
  }
  for (const [file, promises] of bySource) {
    targets.push({
      file,
      inner: renderBlock(matrix, {
        surfaces: surfaceKeys,
        promises,
        intro:
          GEN_NOTE +
          "\n>\n> The public promises this document makes, and the status of each surface.",
      }),
    });
  }

  // 3. Driver READMEs — the driver_helpers column for every promise. Globbed
  //    so a new driver is covered automatically once it has a README.
  const driverReadmes = [];
  const clientReadme = "crates/reddb-client/README.md";
  if (fs.existsSync(path.resolve(repoRoot, clientReadme))) driverReadmes.push(clientReadme);
  const driversDir = path.resolve(repoRoot, "drivers");
  if (fs.existsSync(driversDir)) {
    for (const entry of fs.readdirSync(driversDir).sort()) {
      const rel = `drivers/${entry}/README.md`;
      if (fs.existsSync(path.resolve(repoRoot, rel))) driverReadmes.push(rel);
    }
  }
  for (const file of driverReadmes) {
    targets.push({
      file,
      inner: renderBlock(matrix, {
        surfaces: ["driver_helpers"],
        promises: matrix.promises,
        intro:
          GEN_NOTE +
          "\n>\n> Driver-helper (SDK Helper Spec v1.0) support for every public promise. " +
          "A helper not marked supported here is not promised by this driver.",
      }),
    });
  }

  return targets;
}

function main() {
  const mode = process.argv.includes("--write") ? "write" : "check";
  let matrix;
  try {
    matrix = loadMatrix(path.resolve(repoRoot, MATRIX_REL));
  } catch (err) {
    console.error(`docs-matrix: cannot load matrix: ${err.message}`);
    process.exit(2);
  }

  const targets = buildTargets(matrix);
  const drift = [];
  let written = 0;

  for (const { file, inner } of targets) {
    const abs = path.resolve(repoRoot, file);
    if (!fs.existsSync(abs)) {
      drift.push(`${file}: target file does not exist`);
      continue;
    }
    const current = fs.readFileSync(abs, "utf8");
    const next = applyBlock(current, inner);
    if (mode === "write") {
      if (next !== current) {
        fs.writeFileSync(abs, next);
        written++;
        console.log(`docs-matrix: updated ${file}`);
      }
    } else if (next !== current) {
      const had = extractBlock(current) !== null;
      drift.push(
        had
          ? `${file}: contract-matrix block is out of date`
          : `${file}: missing contract-matrix block`,
      );
    }
  }

  if (mode === "write") {
    console.log(
      `docs-matrix: ${targets.length} target(s) checked, ${written} updated.`,
    );
    process.exit(0);
  }

  console.log(`docs-matrix: ${targets.length} doc target(s) checked against ${MATRIX_REL}.`);
  if (drift.length > 0) {
    console.error(`\ndocs-matrix: ${drift.length} doc(s) out of sync with the contract matrix:`);
    for (const d of drift) console.error(`  - ${d}`);
    console.error(
      "\nFix: run `node scripts/gen-docs-from-matrix.mjs --write` and commit, so docs\n" +
        "match the matrix. Docs cannot promise more than the matrix marks supported.",
    );
    process.exit(1);
  }
  console.log("docs-matrix: OK — every public doc matches the contract matrix.");
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
