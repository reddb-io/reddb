import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import {
  SCHEMA,
  checkValidationDiff,
  extractTestClaims,
  loadValidationRecords,
  testPathPattern,
} from "./check-afk-validation-diff.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const CHECKER = "scripts/check-afk-validation-diff.mjs";

function git(repo, args) {
  return execFileSync("git", ["-C", repo, ...args], { encoding: "utf8" });
}

function write(repo, relativePath, contents) {
  const fullPath = path.join(repo, relativePath);
  fs.mkdirSync(path.dirname(fullPath), { recursive: true });
  fs.writeFileSync(fullPath, contents);
}

function makeRepo() {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "afk-validation-diff-"));
  git(repo, ["init", "-q"]);
  git(repo, ["config", "user.email", "test@example.invalid"]);
  git(repo, ["config", "user.name", "Test User"]);
  write(repo, "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "base"]);
  return repo;
}

function sidecar(repo, records) {
  const sidecarPath = path.join(repo, "validation.jsonl");
  fs.writeFileSync(sidecarPath, records.map((record) => JSON.stringify(record)).join("\n") + "\n");
  return sidecarPath;
}

function passedTestRecord(overrides = {}) {
  return {
    schema: SCHEMA,
    name: "test:root",
    command: "pnpm -C /repo test",
    status: "passed",
    durationMs: 12,
    summary: "command exited 0",
    ...overrides,
  };
}

function runChecker(repo, sidecarPath) {
  return execFileSync("node", [CHECKER, "--repo", repo, "--sidecar", sidecarPath, "--base", "HEAD~1", "--head", "HEAD"], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: "pipe",
  });
}

test("test path matcher covers repo test file conventions", () => {
  for (const filePath of [
    "tests/e2e_meta_json_sidecar_policy.rs",
    "scripts/check-afk-validation-diff.test.mjs",
    "drivers/js/test/smoke.test.mjs",
    "drivers/python/tests/test_params.py",
    "crates/reddb-wire/tests/parser_table.rs",
  ]) {
    assert.equal(testPathPattern(filePath), true, `${filePath} should be test evidence`);
  }
  assert.equal(testPathPattern("src/runtime.rs"), false);
});

test("loads only structured red.afk.validation.v1 sidecar records", () => {
  const repo = makeRepo();
  try {
    const sidecarPath = sidecar(repo, [passedTestRecord()]);
    const records = loadValidationRecords(sidecarPath);
    assert.equal(records.length, 1);
    assert.equal(records[0].schema, SCHEMA);

    const badPath = path.join(repo, "bad-validation.jsonl");
    fs.writeFileSync(badPath, JSON.stringify({ schema: "not-prose", name: "test:root" }) + "\n");
    assert.throws(() => loadValidationRecords(badPath), /expected red\.afk\.validation\.v1/);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("extracts passed test claims from structured test records", () => {
  const claims = extractTestClaims([
    passedTestRecord({ name: "test:root" }),
    passedTestRecord({ name: "typecheck:root", command: "cargo check", status: "passed" }),
    passedTestRecord({ name: "test:root", status: "failed" }),
  ]);
  assert.equal(claims.length, 1);
  assert.equal(claims[0].name, "test:root");
});

test("flags a passed test claim when no tests are present in the diff", () => {
  const repo = makeRepo();
  try {
    write(repo, "src/lib.rs", "pub fn value() -> u32 { 2 }\n");
    git(repo, ["add", "."]);
    git(repo, ["commit", "-qm", "code only"]);
    const sidecarPath = sidecar(repo, [passedTestRecord()]);

    assert.throws(() => runChecker(repo, sidecarPath), /no matching test file or inline test addition exists/);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("passes when a claimed root test has a test file in the diff", () => {
  const repo = makeRepo();
  try {
    write(repo, "scripts/check-afk-validation-diff.test.mjs", "import { test } from 'node:test';\n");
    git(repo, ["add", "."]);
    git(repo, ["commit", "-qm", "add test"]);
    const sidecarPath = sidecar(repo, [passedTestRecord()]);

    assert.match(runChecker(repo, sidecarPath), /AFK validation diff check passed/);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("passes when a claimed root test adds inline Rust test evidence", () => {
  const repo = makeRepo();
  try {
    write(
      repo,
      "src/lib.rs",
      "pub fn value() -> u32 { 2 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn value_is_two() {\n        assert_eq!(super::value(), 2);\n    }\n}\n",
    );
    git(repo, ["add", "."]);
    git(repo, ["commit", "-qm", "add inline test"]);
    const sidecarPath = sidecar(repo, [passedTestRecord()]);

    assert.match(runChecker(repo, sidecarPath), /AFK validation diff check passed/);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("does not parse free-form validation prose as a test claim", () => {
  const result = checkValidationDiff({
    records: [
      {
        schema: SCHEMA,
        name: "typecheck:root",
        command: "cargo check --workspace --locked",
        status: "passed",
      },
    ],
    diff: {
      files: ["agent-notes.md"],
      patch: "+I validated scripts/check-afk-validation-diff.test.mjs\n",
    },
  });
  assert.equal(result.claims.length, 0);
  assert.equal(result.violations.length, 0);
});

test("CI runs the AFK validation sidecar contract test and optional diff check", () => {
  const ci = fs.readFileSync(path.join(repoRoot, ".github/workflows/ci.yml"), "utf8");
  assert.match(ci, /afk-validation-sidecar:/);
  assert.match(ci, /node --test scripts\/check-afk-validation-diff\.test\.mjs/);
  assert.match(ci, /RED_AFK_VALIDATION_SIDECAR/);
});
