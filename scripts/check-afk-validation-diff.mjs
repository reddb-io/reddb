#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const SCHEMA = "red.afk.validation.v1";

function usage() {
  return `Usage: node scripts/check-afk-validation-diff.mjs --sidecar PATH [--base REF] [--head REF] [--repo DIR] [--allow-missing-sidecar]

Checks that passed test claims from the ${SCHEMA} JSONL sidecar have matching
test evidence in the git diff.`;
}

function parseArgs(argv) {
  const args = {
    sidecar: process.env.RED_AFK_VALIDATION_SIDECAR || "",
    base: process.env.RED_AFK_VALIDATION_BASE || "origin/main",
    head: process.env.RED_AFK_VALIDATION_HEAD || "HEAD",
    repo: process.cwd(),
    allowMissingSidecar: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      args.help = true;
    } else if (arg === "--sidecar") {
      args.sidecar = argv[++i] || "";
    } else if (arg === "--base") {
      args.base = argv[++i] || "";
    } else if (arg === "--head") {
      args.head = argv[++i] || "";
    } else if (arg === "--repo") {
      args.repo = argv[++i] || "";
    } else if (arg === "--allow-missing-sidecar") {
      args.allowMissingSidecar = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

function loadValidationRecords(sidecarPath) {
  const text = fs.readFileSync(sidecarPath, "utf8");
  const records = [];
  const lines = text.split(/\r?\n/).filter((line) => line.trim() !== "");

  for (const [index, line] of lines.entries()) {
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch (err) {
      throw new Error(`invalid JSON on sidecar line ${index + 1}: ${err.message}`);
    }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      throw new Error(`sidecar line ${index + 1} is not a JSON object`);
    }
    if (parsed.schema !== SCHEMA) {
      throw new Error(`sidecar line ${index + 1} has schema ${JSON.stringify(parsed.schema)}, expected ${SCHEMA}`);
    }
    records.push(parsed);
  }

  if (records.length === 0) {
    throw new Error(`sidecar ${sidecarPath} contains no ${SCHEMA} records`);
  }

  return records;
}

function shellWords(command) {
  if (!command) return [];
  return command
    .match(/"[^"]*"|'[^']*'|\S+/g)
    ?.map((word) => word.replace(/^['"]|['"]$/g, ""))
    ?? [];
}

function commandLooksLikeTest(command) {
  return /\b(cargo\s+test|node\s+--test|pnpm\b[^\n;]*\btest\b|npm\b[^\n;]*\btest\b|yarn\b[^\n;]*\btest\b|pytest\b|go\s+test\b|vitest\b|jest\b)/.test(
    command || "",
  );
}

function testPathPattern(filePath) {
  const normalized = filePath.replace(/\\/g, "/");
  return (
    /(^|\/)(tests?|__tests__|spec)\//.test(normalized) ||
    /(^|\/)[^/]+[._-](test|spec)\.(cjs|mjs|js|jsx|ts|tsx|rs|py|go|java|kt|rb|php|zig|dart)$/.test(normalized) ||
    /(^|\/)test_[^/]+\.(py|rs)$/.test(normalized) ||
    /(^|\/)[^/]+_test\.go$/.test(normalized)
  );
}

function extractExplicitTestPaths(record) {
  const values = [record.name, ...shellWords(record.command)].filter(Boolean);
  return values
    .map((value) => value.replace(/^\.?\//, ""))
    .filter((value) => value.includes("/") || /\.[a-z0-9]+$/i.test(value))
    .filter(testPathPattern);
}

function extractTestClaims(records) {
  return records
    .filter((record) => record.status === "passed")
    .filter((record) => {
      const name = typeof record.name === "string" ? record.name : "";
      return name.startsWith("test:") || commandLooksLikeTest(String(record.command || ""));
    })
    .map((record) => {
      const name = String(record.name || "");
      const rawScope = name.startsWith("test:") ? name.slice("test:".length) : "root";
      const scope = rawScope === "" || rawScope === "root" || rawScope === "no-package" ? "." : rawScope;
      return {
        name,
        scope,
        command: String(record.command || ""),
        explicitPaths: extractExplicitTestPaths(record),
      };
    });
}

function gitOutput(repo, args) {
  return execFileSync("git", ["-C", repo, ...args], { encoding: "utf8" });
}

function diffArgs(base, head) {
  return [`${base}...${head}`];
}

function readDiff(repo, base, head) {
  try {
    return {
      files: gitOutput(repo, ["diff", "--name-only", "--diff-filter=ACMR", ...diffArgs(base, head)])
        .split(/\r?\n/)
        .filter(Boolean),
      patch: gitOutput(repo, ["diff", "--unified=0", "--no-ext-diff", ...diffArgs(base, head)]),
    };
  } catch {
    return {
      files: gitOutput(repo, ["diff", "--name-only", "--diff-filter=ACMR", base, head])
        .split(/\r?\n/)
        .filter(Boolean),
      patch: gitOutput(repo, ["diff", "--unified=0", "--no-ext-diff", base, head]),
    };
  }
}

function addedInlineTestPaths(patch) {
  const paths = new Set();
  let current = "";
  for (const line of patch.split(/\r?\n/)) {
    if (line.startsWith("+++ b/")) {
      current = line.slice("+++ b/".length);
      continue;
    }
    if (!line.startsWith("+") || line.startsWith("+++")) continue;
    if (
      /#\[(tokio::)?test\]/.test(line) ||
      /#\[cfg\(test\)\]/.test(line) ||
      /\bmod tests\b/.test(line) ||
      /^\+\s*(test|it|describe)\s*\(/.test(line) ||
      /^\+\s*@Test\b/.test(line) ||
      /^\+\s*func Test[A-Z_]/.test(line)
    ) {
      if (current) paths.add(current);
    }
  }
  return paths;
}

function buildTestEvidence(diff) {
  const evidence = new Set(diff.files.filter(testPathPattern));
  for (const filePath of addedInlineTestPaths(diff.patch)) evidence.add(filePath);
  return evidence;
}

function pathMatchesScope(filePath, scope) {
  if (scope === ".") return true;
  const normalized = scope.replace(/\\/g, "/").replace(/^\.?\//, "").replace(/\/$/, "");
  return filePath === normalized || filePath.startsWith(`${normalized}/`);
}

function hasEvidenceForClaim(claim, evidence) {
  if (claim.explicitPaths.length > 0) {
    return claim.explicitPaths.some((explicitPath) => evidence.has(explicitPath));
  }
  return [...evidence].some((filePath) => pathMatchesScope(filePath, claim.scope));
}

function checkValidationDiff({ records, diff }) {
  const claims = extractTestClaims(records);
  const evidence = buildTestEvidence(diff);
  const violations = claims.filter((claim) => !hasEvidenceForClaim(claim, evidence));
  return { claims, evidence: [...evidence].sort(), violations };
}

function ghaError(message) {
  if (process.env.GITHUB_ACTIONS === "true") {
    console.error(`::error::${message}`);
  } else {
    console.error(`ERROR: ${message}`);
  }
}

function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (err) {
    ghaError(err.message);
    console.error(usage());
    return 2;
  }

  if (args.help) {
    console.log(usage());
    return 0;
  }

  if (!args.sidecar) {
    if (args.allowMissingSidecar) {
      console.log("AFK validation diff check skipped: no sidecar path supplied");
      return 0;
    }
    ghaError("missing --sidecar PATH or RED_AFK_VALIDATION_SIDECAR");
    return 2;
  }

  const sidecarPath = path.resolve(args.repo, args.sidecar);
  if (!fs.existsSync(sidecarPath)) {
    if (args.allowMissingSidecar) {
      console.log(`AFK validation diff check skipped: sidecar not found at ${sidecarPath}`);
      return 0;
    }
    ghaError(`sidecar not found: ${sidecarPath}`);
    return 2;
  }

  try {
    const records = loadValidationRecords(sidecarPath);
    const diff = readDiff(args.repo, args.base, args.head);
    const result = checkValidationDiff({ records, diff });

    if (result.claims.length === 0) {
      console.log(`AFK validation diff check passed: ${SCHEMA} sidecar contains no passed test claims`);
      return 0;
    }

    if (result.violations.length > 0) {
      for (const violation of result.violations) {
        ghaError(
          `${SCHEMA} sidecar claims passed test ${JSON.stringify(violation.name)} but no matching test file or inline test addition exists in diff ${args.base}...${args.head}`,
        );
      }
      if (result.evidence.length === 0) {
        console.error("No test evidence paths were found in the diff.");
      } else {
        console.error(`Test evidence paths found: ${result.evidence.join(", ")}`);
      }
      return 1;
    }

    console.log(
      `AFK validation diff check passed: ${result.claims.length} passed test claim(s), ${result.evidence.length} diff test evidence path(s)`,
    );
    return 0;
  } catch (err) {
    ghaError(err.message);
    return 2;
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  process.exitCode = main();
}

export {
  SCHEMA,
  buildTestEvidence,
  checkValidationDiff,
  extractTestClaims,
  loadValidationRecords,
  testPathPattern,
};
