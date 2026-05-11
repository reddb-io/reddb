import assert from "node:assert/strict";
import cp from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");
const issuesPath = process.env.REDDB_ISSUES_RAW || "/tmp/reddb_issues_raw.json";

function runReport() {
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), "reddb-evidence-report-"));
  cp.execFileSync("node", ["scripts/issue_code_evidence_report.js", issuesPath, outDir], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

  const reportPath = path.join(outDir, "github_issues_code_evidence_status.json");
  return JSON.parse(fs.readFileSync(reportPath, "utf8"));
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
  const repeatReport = runReport();
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
