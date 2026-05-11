import assert from "node:assert/strict";
import cp from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");
const issuesPath = process.env.REDDB_ISSUES_RAW || "/tmp/reddb_issues_raw.json";

let cachedReport = null;

function runFreshReport() {
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), "reddb-evidence-report-"));
  cp.execFileSync("node", ["scripts/issue_code_evidence_report.js", issuesPath, outDir], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

  const reportPath = path.join(outDir, "github_issues_code_evidence_status.json");
  return JSON.parse(fs.readFileSync(reportPath, "utf8"));
}

function runReport() {
  if (!cachedReport) cachedReport = runFreshReport();
  return cachedReport;
}

function stableLedger(report) {
  return report.issues.map((issue) => ({
    number: issue.number,
    status: issue.resolution.status,
    final_disposition: issue.final_disposition,
  }));
}

function issueByNumber(report, number) {
  return report.issues.find((issue) => issue.number === number);
}

test("issue evidence report has reproducible final dispositions", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const repeatReport = runFreshReport();
  const numbers = report.issues.map((issue) => issue.number);
  const uniqueNumbers = new Set(numbers);

  assert.equal(report.summary.total, 311);
  assert.equal(report.issues.length, 311);
  assert.equal(uniqueNumbers.size, 311);
  assert.deepEqual(report.summary.by_status, repeatReport.summary.by_status);
  assert.deepEqual(stableLedger(report), stableLedger(repeatReport));

  for (const outcome of ["confirmed", "superseded", "reopened", "split"]) {
    assert.equal(typeof report.semantics.final_dispositions[outcome], "string");
  }

  for (const issue of report.issues) {
    assert.ok(["confirmed", "superseded", "reopened", "split"].includes(issue.final_disposition.outcome));
    assert.equal(typeof issue.final_disposition.placeholder, "boolean");
    assert.equal(typeof issue.final_disposition.reason, "string");
  }

  const placeholderStatuses = new Set([
    "code_evidence_partial",
    "code_evidence_partial_github_open",
    "code_evidence_confirmed_github_open",
  ]);
  const placeholders = report.issues.filter((issue) => placeholderStatuses.has(issue.resolution.status));

  assert.equal(report.summary.placeholder_final_dispositions, placeholders.length);
  assert.ok(placeholders.every((issue) => issue.final_disposition.placeholder));
  assert.ok(
    placeholders.every((issue) =>
      ["confirmed", "reopened"].includes(issue.final_disposition.outcome),
    ),
  );
  assert.deepEqual(
    report.issues
      .filter((issue) => issue.resolution.status === "code_evidence_confirmed_github_open")
      .map((issue) => issue.number)
      .sort((a, b) => a - b),
    [238, 252, 282],
  );
});

test("migration evidence closure records final dispositions for issue 335 scope", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const issue16 = issueByNumber(report, 16);
  const issue21 = issueByNumber(report, 21);
  const issue24 = issueByNumber(report, 24);

  assert.equal(issue16.final_disposition.outcome, "confirmed");
  assert.equal(issue16.final_disposition.placeholder, false);

  assert.equal(issue21.final_disposition.outcome, "split");
  assert.equal(issue21.final_disposition.placeholder, false);
  assert.deepEqual(issue21.final_disposition.split_into, [346]);

  assert.equal(issue24.final_disposition.outcome, "superseded");
  assert.equal(issue24.final_disposition.placeholder, false);
  assert.match(issue24.final_disposition.reason, /crates\/reddb-server\/src\/runtime\/impl_migrations\.rs/);

  for (const issue of [issue16, issue21, issue24]) {
    assert.notEqual(issue.resolution.status, "code_evidence_partial");
    assert.notEqual(issue.resolution.status, "code_evidence_partial_github_open");
  }
});

test("release tooling evidence closure records final dispositions for issue 337 scope", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const issue62 = issueByNumber(report, 62);
  const issue68 = issueByNumber(report, 68);
  const issue116 = issueByNumber(report, 116);

  for (const issue of [issue62, issue68, issue116]) {
    assert.equal(issue.final_disposition.outcome, "confirmed");
    assert.equal(issue.final_disposition.placeholder, false);
    assert.notEqual(issue.resolution.status, "code_evidence_partial");
    assert.notEqual(issue.resolution.status, "code_evidence_partial_github_open");
  }

  assert.match(issue62.final_disposition.reason, /scripts\/check-red-client-size\.sh/);
  assert.match(issue68.final_disposition.reason, /Dockerfile\.client/);
  assert.match(issue116.final_disposition.reason, /make drill-nightly/);
});

test("parser hardening evidence closure records final dispositions for issue 338 scope", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const issue87 = issueByNumber(report, 87);
  const issue97 = issueByNumber(report, 97);
  const issue231 = issueByNumber(report, 231);
  const issue233 = issueByNumber(report, 233);
  const issue236 = issueByNumber(report, 236);

  for (const issue of [issue87, issue97, issue231, issue233, issue236]) {
    assert.equal(issue.final_disposition.outcome, "confirmed");
    assert.equal(issue.final_disposition.placeholder, false);
    assert.notEqual(issue.resolution.status, "code_evidence_partial");
    assert.notEqual(issue.resolution.status, "code_evidence_partial_github_open");
  }

  assert.match(issue97.final_disposition.reason, /snapshot_redaction_lint\.rs/);
  assert.match(issue97.final_disposition.reason, /secret_redactor\.rs/);
  assert.match(issue231.final_disposition.reason, /tests\/conformance\.rs/);
  assert.match(issue231.final_disposition.reason, /tests\/conformance/);
});

test("statement execution evidence closure records final dispositions for issue 336 scope", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const issues = [46, 48, 49, 50, 51, 52].map((number) => issueByNumber(report, number));

  for (const issue of issues) {
    assert.equal(issue.final_disposition.outcome, "confirmed");
    assert.equal(issue.final_disposition.placeholder, false);
    assert.notEqual(issue.resolution.status, "code_evidence_partial");
    assert.notEqual(issue.resolution.status, "code_evidence_partial_github_open");
  }

  assert.match(issues[0].final_disposition.reason, /e2e_statement_execution_contract\.rs/);
  assert.match(issues[1].final_disposition.reason, /permission denied/);
  assert.match(issues[2].final_disposition.reason, /APPEND ONLY/);
  assert.match(issues[4].final_disposition.reason, /UPDATE and DELETE/);
});

test("red schema reference closure records final dispositions for issue 341 scope", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const issue163 = issueByNumber(report, 163);
  const issue263 = issueByNumber(report, 263);

  for (const issue of [issue163, issue263]) {
    assert.equal(issue.final_disposition.outcome, "confirmed");
    assert.equal(issue.final_disposition.placeholder, false);
    assert.notEqual(issue.resolution.status, "code_evidence_partial");
    assert.notEqual(issue.resolution.status, "code_evidence_partial_github_open");
  }

  assert.match(issue163.final_disposition.reason, /docs\/perf\/wins\.md/);
  assert.match(issue163.final_disposition.reason, /duel-official/);
  assert.match(issue263.final_disposition.reason, /docs\/reference\/red-schema\.md/);
  assert.match(issue263.final_disposition.reason, /tests\/e2e_red_schema\.rs/);
});

test("DDL auth closure records final disposition for issue 344 scope", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const issue309 = issueByNumber(report, 309);

  assert.equal(issue309.final_disposition.outcome, "confirmed");
  assert.equal(issue309.final_disposition.placeholder, false);
  assert.notEqual(issue309.resolution.status, "code_evidence_partial");
  assert.notEqual(issue309.resolution.status, "code_evidence_partial_github_open");
  assert.match(issue309.final_disposition.reason, /tests\/iam_policy_runtime\.rs/);
  assert.match(issue309.final_disposition.reason, /DROP and TRUNCATE/);
  assert.match(issue309.final_disposition.reason, /audit/);
});

test("SDK and Redis migration tooling closure records final dispositions for issue 340 scope", () => {
  assert.ok(fs.existsSync(issuesPath), `${issuesPath} must exist`);

  const report = runReport();
  const issue197 = issueByNumber(report, 197);
  const issue199 = issueByNumber(report, 199);

  assert.equal(issue197.final_disposition.outcome, "confirmed");
  assert.equal(issue197.final_disposition.placeholder, false);
  assert.notEqual(issue197.resolution.status, "code_evidence_partial");
  assert.notEqual(issue197.resolution.status, "code_evidence_partial_github_open");
  assert.match(issue197.final_disposition.reason, /drivers\/python\/tests\/test_cache\.py/);
  assert.match(issue197.final_disposition.reason, /cache\.get, cache\.put, and cache\.invalidate/);

  assert.equal(issue199.final_disposition.outcome, "split");
  assert.equal(issue199.final_disposition.placeholder, false);
  assert.deepEqual(issue199.final_disposition.split_into, [347]);
  assert.notEqual(issue199.resolution.status, "code_evidence_partial");
  assert.notEqual(issue199.resolution.status, "code_evidence_partial_github_open");
  assert.match(issue199.final_disposition.reason, /red migrate-from-redis/);
  assert.match(issue199.final_disposition.reason, /docs\/guides\/migrate-redis-to-blob-cache\.md/);
});
