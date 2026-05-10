#!/usr/bin/env node

const cp = require("child_process");
const fs = require("fs");
const path = require("path");

const issuesPath = process.argv[2] || "/tmp/reddb_issues_raw.json";
const outDir = process.argv[3] || "reports";

const SQL_SINGLE = new Set(
  "SELECT FROM WHERE WITH CREATE DROP APPLY ROLLBACK EXPLAIN MIGRATION TABLE QUEUE MOVE INSERT UPDATE DELETE VALUES SET SHOW ALTER TRUNCATE GET PUT WATCH CAS INCR DECR TTL IF EXISTS NOT NULL AND OR BY ON TO FOR AS INTO".split(
    " ",
  ),
);

const GENERIC = new Set(
  "red reddb rdb sql api json cli http grpc mcp wire driver drivers test tests docs doc cargo check build run crate crates src lib mod readme users user email name names value values key keys status error result results command commands query queries collection collections table queue config vault graph vector document runtime parser planner executor storage engine cache policy policies action actions events event mode model models data row rows field fields path paths type types kind kinds item items format formats payload request response tenant tenants source target default existing new old same implementation issue parent slice objective acceptance criteria blocked none start immediately".split(
    " ",
  ),
);

const STOP = new Set(
  "the and for with from into out via all add can has have had not are was were this that when then these those where which every across under over normal true false null uma para com que por dos das como esse essa este esta objetivo build what".split(
    " ",
  ),
);

function read(file) {
  try {
    const stat = fs.statSync(file);
    if (stat.size > 1_500_000) return null;
    return fs.readFileSync(file, "utf8");
  } catch {
    return null;
  }
}

function walk(dir) {
  if (!fs.existsSync(dir)) return [];
  const out = [];
  const entries = fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name));
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full));
    if (entry.isFile() && entry.name.endsWith(".md")) out.push(full);
  }
  return out;
}

function fileKind(file) {
  if (file.endsWith(".md")) return "doc";
  if (file.startsWith("tests/") || /(^|\/)tests\//.test(file) || /_test\.rs$|\.test\.|\.spec\./.test(file)) {
    return "test";
  }
  if (file.startsWith(".github/")) return "ci";
  if (/Cargo\.toml$|Makefile$/.test(file)) return "build";
  return "code";
}

function workflowState(file) {
  const parts = file.split(/[\\/]/);
  return parts.length === 2 ? "root" : parts[1];
}

function codeSpans(text) {
  return [...text.matchAll(/`([^`\n]{2,160})`/g)].map((m) => m[1].trim());
}

function acceptanceItems(text) {
  const out = [];
  for (const line of text.split(/\r?\n/)) {
    const match = line.match(/^\s*-\s*\[[ xX]\]\s*(.+)$/);
    if (match) out.push(match[1]);
  }
  return out;
}

function section(text, heading) {
  const lines = text.split(/\r?\n/);
  const out = [];
  for (let i = 0; i < lines.length; i += 1) {
    if (!new RegExp(`^##\\s+${heading}\\b`, "i").test(lines[i])) continue;
    for (let j = i + 1; j < lines.length && !/^##\s+/.test(lines[j]); j += 1) {
      out.push(lines[j]);
    }
  }
  return out.join("\n");
}

function cleanup(term) {
  const cleaned = (term || "")
    .trim()
    .replace(/^['"“”]+|['"“”.,;:]+$/g, "")
    .replace(/\s+/g, " ");
  if (!cleaned || cleaned.length > 140) return "";
  if (/^https?:/.test(cleaned) || /^#?\d+$/.test(cleaned)) return "";
  return cleaned;
}

function looksLikePath(term) {
  return (
    /(^|\b)(src|crates|tests|docs|drivers|examples|\.github)\//.test(term) ||
    /\.(rs|toml|md|ts|tsx|js|mjs|py|go|yml|yaml|json|proto|cpp|hpp|php)$/.test(term)
  );
}

function looksLikeCommand(term) {
  return /^(cargo|make|gh|jq|git|npm|pnpm|python|node|curl)\b/.test(term.trim());
}

function termScore(raw) {
  const term = cleanup(raw);
  if (!term) return 0;
  const lower = term.toLowerCase();
  const upper = term.toUpperCase();
  if (looksLikePath(term)) return 9;
  if (term.includes("::") || term.includes("_") || term.includes(":") || /[a-z][A-Z]/.test(term)) return 8;
  if (term.includes("/") && term.length >= 8) return 8;
  if (term.includes(" ")) {
    const words = term.split(" ").filter(Boolean);
    if (words.length >= 2 && !words.every((w) => SQL_SINGLE.has(w.toUpperCase()))) {
      return Math.min(8, 4 + words.length);
    }
    if (words.length >= 2) return 5;
    return 3;
  }
  if (SQL_SINGLE.has(upper)) return 0;
  if (GENERIC.has(lower) || STOP.has(lower)) return 0;
  if (/^[A-Z0-9_]{3,}$/.test(term)) return 4;
  if (term.length >= 8) return 4;
  if (term.length >= 5) return 2;
  return 0;
}

function addTerm(map, term, source, weight) {
  const cleaned = cleanup(term);
  const score = termScore(cleaned);
  if (score <= 0) return;
  const key = cleaned.toLowerCase();
  if (!map.has(key)) map.set(key, { term: cleaned, sources: new Set(), score, weight: 0 });
  map.get(key).sources.add(source);
  map.get(key).weight += weight;
}

function candidateTerms(issue, localFiles) {
  const text = [issue.title, issue.body || "", ...localFiles.map((f) => f.text)].join("\n");
  const terms = new Map();

  for (const span of codeSpans(text)) {
    if (looksLikeCommand(span)) continue;
    addTerm(terms, span, looksLikePath(span) ? "path_reference" : "code_span", 5);
    for (const match of span.matchAll(/[A-Za-z_][A-Za-z0-9_:./-]{4,}/g)) {
      addTerm(terms, match[0].replace(/[.:]+$/, ""), "code_span_token", 2);
    }
  }

  for (const item of acceptanceItems(text).slice(0, 20)) {
    for (const span of codeSpans(item)) addTerm(terms, span, "acceptance_code_span", 6);
    for (const match of item.matchAll(/[A-Za-z_][A-Za-z0-9_:./-]{5,}/g)) {
      addTerm(terms, match[0].replace(/[.:]+$/, ""), "acceptance_keyword", 1);
    }
  }

  for (const heading of ["What to build", "Problem Statement", "Scope Clarification"]) {
    for (const span of codeSpans(section(text, heading).slice(0, 2500))) {
      addTerm(terms, span, "objective_code_span", 5);
    }
  }

  const titleWords = issue.title.match(/[A-Za-z][A-Za-z0-9_-]{5,}/g) || [];
  for (const word of titleWords) addTerm(terms, word, "title_keyword", 2);
  for (let i = 0; i < titleWords.length - 1; i += 1) {
    addTerm(terms, `${titleWords[i]} ${titleWords[i + 1]}`, "title_phrase", 3);
  }
  for (const match of text.matchAll(/\b[A-Z][A-Z0-9_]{3,}\b/g)) addTerm(terms, match[0], "acronym", 2);

  return [...terms.values()]
    .map((term) => ({ term: term.term, sources: [...term.sources], score: term.score, weight: term.weight }))
    .sort((a, b) => {
      const score = b.score + b.weight - (a.score + a.weight);
      if (score !== 0) return score;
      return a.term.localeCompare(b.term);
    })
    .slice(0, 36);
}

function buildParents(issues, localByIssue) {
  const parents = new Map();
  const children = new Map();
  for (const issue of issues) {
    const text = [issue.body || "", ...(localByIssue.get(issue.number) || []).map((f) => f.text)].join("\n");
    const parentNums = new Set();
    for (const match of text.matchAll(/(?:Parent|Parent PRD)\s*(?:\n|:)?\s*(?:reddb-io\/reddb)?#(\d+)/gi)) {
      const number = Number(match[1]);
      if (number && number !== issue.number) parentNums.add(number);
    }
    const lines = text.split(/\r?\n/);
    for (let i = 0; i < lines.length; i += 1) {
      if (!/^##\s+Parent\b/i.test(lines[i])) continue;
      for (let j = i + 1; j < Math.min(lines.length, i + 6); j += 1) {
        for (const match of (lines[j] || "").matchAll(/#(\d+)/g)) {
          const number = Number(match[1]);
          if (number && number !== issue.number) parentNums.add(number);
        }
      }
    }
    parents.set(issue.number, [...parentNums].sort((a, b) => a - b));
    for (const parent of parentNums) {
      if (!children.has(parent)) children.set(parent, []);
      children.get(parent).push(issue.number);
    }
  }
  return { parents, children };
}

function descendantsOf(number, children, seen = new Set()) {
  for (const child of children.get(number) || []) {
    if (seen.has(child)) continue;
    seen.add(child);
    descendantsOf(child, children, seen);
  }
  return [...seen].sort((a, b) => a - b);
}

function evidenceRank(evidence) {
  const kindRank = { code: 0, test: 1, build: 2, ci: 3, doc: 4 }[evidence.kind] ?? 5;
  return kindRank * 10 - (evidence.term_score || 0);
}

function main() {
  fs.mkdirSync(outDir, { recursive: true });

  const issues = JSON.parse(fs.readFileSync(issuesPath, "utf8"));
  const roots = ["src", "crates", "tests", "docs", "drivers", "examples", ".github", "Cargo.toml", "Makefile"].filter((root) =>
    fs.existsSync(root),
  );
  const indexedPaths = cp
    .execFileSync("rg", ["--files", ...roots], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 })
    .split(/\r?\n/)
    .filter(Boolean)
    .filter((file) => !/(^|\/)target\//.test(file))
    .filter((file) => !/\.(png|jpg|jpeg|gif|webp|ico|lock|bin|db|wal)$/i.test(file))
    .sort();

  const fileIndex = indexedPaths
    .map((file) => ({ file, text: read(file), kind: fileKind(file) }))
    .filter((file) => file.text != null)
    .map((file) => ({ ...file, lower: file.text.toLowerCase(), lines: file.text.split(/\r?\n/) }));

  const localByIssue = new Map();
  for (const file of walk("issues")) {
    const match = path.basename(file).match(/^(\d+)-/);
    if (!match) continue;
    const number = Number(match[1]);
    if (!localByIssue.has(number)) localByIssue.set(number, []);
    localByIssue.get(number).push({ path: file, workflow_state: workflowState(file), text: read(file) || "" });
  }

  const { parents, children } = buildParents(issues, localByIssue);

  function search(term, max = 4) {
    const lower = term.toLowerCase();
    const out = [];
    for (const indexed of fileIndex) {
      if (!indexed.lower.includes(lower)) continue;
      for (let i = 0; i < indexed.lines.length; i += 1) {
        const line = indexed.lines[i];
        if (!line.toLowerCase().includes(lower)) continue;
        out.push({
          type: "current_code_match",
          path: indexed.file,
          line: i + 1,
          kind: indexed.kind,
          matched_term: term,
          snippet: line.trim().slice(0, 240),
        });
        break;
      }
      if (out.length >= max) break;
    }
    return out;
  }

  function evidenceFor(issue) {
    const localFiles = localByIssue.get(issue.number) || [];
    const terms = candidateTerms(issue, localFiles);
    const evidence = [];
    const seen = new Set();
    const add = (record, term) => {
      const key = `${record.path}|${record.line || 0}|${record.matched_term}`;
      if (seen.has(key)) return;
      seen.add(key);
      evidence.push({ ...record, term_score: term.score, term_sources: term.sources });
    };

    for (const term of terms) {
      if (looksLikePath(term.term) && fs.existsSync(term.term)) {
        add(
          {
            type: "referenced_path_exists",
            path: term.term,
            kind: fileKind(term.term),
            matched_term: term.term,
            snippet: "Referenced path exists in current workspace.",
          },
          term,
        );
      }
    }
    for (const term of terms) {
      if (term.score < 4) continue;
      for (const match of search(term.term, 3)) add(match, term);
      if (evidence.filter((e) => ["code", "test"].includes(e.kind) && e.term_score >= 5).length >= 8) break;
    }

    evidence.sort((a, b) => {
      const rank = evidenceRank(a) - evidenceRank(b);
      if (rank !== 0) return rank;
      const pathRank = a.path.localeCompare(b.path);
      if (pathRank !== 0) return pathRank;
      const lineRank = (a.line || 0) - (b.line || 0);
      if (lineRank !== 0) return lineRank;
      return a.matched_term.localeCompare(b.matched_term);
    });
    return { searched_terms: terms.slice(0, 24), evidence: evidence.slice(0, 14) };
  }

  function manualEvidence(number) {
    if (number === 116) {
      return [
        {
          type: "manual_current_code_match",
          path: "scripts/drill-nightly.sh",
          line: 25,
          kind: "code",
          matched_term: "drill-nightly current-shell eval fix",
          snippet:
            "Run the drill in the current shell so that PATH (sccache, mold, rustup shims) and RUSTC_WRAPPER stay consistent with the runner environment.",
          term_score: 9,
          term_sources: ["github_resolution_comment", "manual_audit"],
        },
        {
          type: "manual_current_code_match",
          path: ".github/workflows/drill-nightly.yml",
          line: 39,
          kind: "ci",
          matched_term: "make drill-nightly",
          snippet: "run: make drill-nightly",
          term_score: 8,
          term_sources: ["github_issue_body", "manual_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "Makefile",
          line: 74,
          kind: "build",
          matched_term: "drill-nightly",
          snippet: "drill-nightly:",
          term_score: 8,
          term_sources: ["github_issue_body", "manual_audit"],
        },
      ];
    }
    if (number === 320) {
      return [
        {
          type: "manual_later_issue_code_match",
          inherited_from_issue: 329,
          path: "crates/reddb-server/src/server/handlers_keyed.rs",
          line: 14,
          kind: "code",
          matched_term: "/v1/config",
          snippet: 'if let Some(rest) = path.strip_prefix("/v1/config/") {',
          term_score: 8,
          term_sources: ["later_issue_329", "manual_audit"],
        },
        {
          type: "manual_later_issue_code_match",
          inherited_from_issue: 329,
          path: "crates/reddb-server/src/server/handlers_keyed.rs",
          line: 17,
          kind: "code",
          matched_term: "/v1/vault",
          snippet: 'if let Some(rest) = path.strip_prefix("/v1/vault/") {',
          term_score: 8,
          term_sources: ["later_issue_329", "manual_audit"],
        },
        {
          type: "manual_later_issue_code_match",
          inherited_from_issue: 329,
          path: "crates/reddb-client/src/lib.rs",
          line: 240,
          kind: "code",
          matched_term: "config() / vault() domain clients",
          snippet: "pub fn config(&self) -> ConfigClient<'_> {",
          term_score: 8,
          term_sources: ["later_issue_329", "manual_audit"],
        },
      ];
    }
    return [];
  }

  const direct = new Map();
  for (const issue of issues) direct.set(issue.number, evidenceFor(issue));

  function classify(issue, directEvidence, inheritedEvidence) {
    const all = [...directEvidence.evidence, ...inheritedEvidence];
    const strong = all.filter((e) => ["code", "test"].includes(e.kind) && e.term_score >= 5);
    const partial = all.filter((e) => ["code", "test"].includes(e.kind) && e.term_score >= 4);
    const doc = all.filter((e) => ["doc", "ci", "build"].includes(e.kind) && e.term_score >= 5);
    const githubOpen = issue.state === "OPEN";
    if (strong.length >= 2) {
      return {
        status: githubOpen ? "code_evidence_confirmed_github_open" : "code_evidence_confirmed",
        objective_reached: true,
        confidence: "high",
      };
    }
    if (strong.length === 1 || partial.length >= 2) {
      return {
        status: githubOpen ? "code_evidence_partial_github_open" : "code_evidence_partial",
        objective_reached: null,
        confidence: "medium",
      };
    }
    if (inheritedEvidence.length > 0) {
      return {
        status: githubOpen ? "covered_by_later_or_child_issue_github_open" : "covered_by_later_or_child_issue",
        objective_reached: true,
        confidence: "medium",
      };
    }
    if (doc.length > 0 && /docs?|documentation/i.test(`${issue.title} ${(issue.labels || []).map((l) => l.name).join(" ")}`)) {
      return {
        status: githubOpen ? "doc_evidence_confirmed_github_open" : "doc_evidence_confirmed",
        objective_reached: true,
        confidence: "medium",
      };
    }
    const localStates = [...new Set((localByIssue.get(issue.number) || []).map((f) => f.workflow_state))];
    if (localStates.includes("done") || issue.state === "CLOSED") {
      return { status: "weak_or_historical_evidence_only", objective_reached: null, confidence: "low" };
    }
    return { status: "no_current_code_evidence_found", objective_reached: false, confidence: "low" };
  }

  function note(status) {
    if (status.includes("confirmed")) return "Current workspace has direct specific code/test evidence.";
    if (status.includes("partial")) {
      return "Current workspace has limited specific code/test evidence; manual review needed for full acceptance coverage.";
    }
    if (status.includes("covered_by")) return "Issue appears covered through later/child issue evidence in current code.";
    if (status === "weak_or_historical_evidence_only") {
      return "Only historical/workflow/weak evidence found; no strong current code proof.";
    }
    return "No current code evidence found by this audit.";
  }

  function finalDisposition(status) {
    if (status === "code_evidence_confirmed_github_open") {
      return {
        outcome: "confirmed",
        placeholder: true,
        reason:
          "Strong current code/test evidence exists, but the GitHub issue is still open and needs reconciliation.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (status === "code_evidence_partial" || status === "code_evidence_partial_github_open") {
      return {
        outcome: "reopened",
        placeholder: true,
        reason:
          "Only partial current code/test evidence exists; a domain slice must confirm, supersede, or split the remaining acceptance criteria.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (status.includes("confirmed")) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason: "Current workspace evidence is strong enough for this generated ledger.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (status.includes("covered_by")) {
      return {
        outcome: "superseded",
        placeholder: false,
        reason: "Current workspace evidence is inherited from later or child issue work.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    return {
      outcome: "reopened",
      placeholder: false,
      reason: "Current workspace evidence is not strong enough to mark this issue confirmed.",
      superseded_by: [],
      reopened_as: [],
      split_into: [],
    };
  }

  const reportIssues = [];
  for (const issue of issues.slice().sort((a, b) => a.number - b.number)) {
    const localFiles = localByIssue.get(issue.number) || [];
    const directEvidence = direct.get(issue.number);
    const descendants = descendantsOf(issue.number, children);
    const inherited = [];
    for (const number of descendants) {
      const childMatches = (direct.get(number)?.evidence || [])
        .filter((e) => ["code", "test"].includes(e.kind) && e.term_score >= 5)
        .slice(0, 2);
      for (const match of childMatches) {
        inherited.push({ ...match, type: "child_or_later_issue_code_match", inherited_from_issue: number });
      }
      if (inherited.length >= 8) break;
    }
    for (const match of manualEvidence(issue.number)) {
      if (match.type.includes("later")) inherited.push(match);
      else directEvidence.evidence.unshift(match);
    }
    const resolution = classify(issue, directEvidence, inherited);
    const disposition = finalDisposition(resolution.status);
    reportIssues.push({
      number: issue.number,
      title: issue.title,
      url: issue.url,
      github_state: issue.state,
      labels: (issue.labels || []).map((label) => label.name).sort(),
      local_workflow_states: [...new Set(localFiles.map((file) => file.workflow_state))].sort(),
      parents: parents.get(issue.number) || [],
      children: (children.get(issue.number) || []).sort((a, b) => a - b),
      descendants,
      resolution: { ...resolution, note: note(resolution.status) },
      final_disposition: disposition,
      current_code_evidence: directEvidence.evidence,
      inherited_or_later_evidence: inherited,
      searched_terms: directEvidence.searched_terms,
    });
  }

  const uniqueIssueNumbers = new Set(reportIssues.map((issue) => issue.number));
  if (uniqueIssueNumbers.size !== reportIssues.length) {
    throw new Error(
      `Evidence report must contain unique issue entries; got ${reportIssues.length} entries and ${uniqueIssueNumbers.size} unique issue numbers`,
    );
  }

  const summary = reportIssues.reduce(
    (acc, issue) => {
      acc.total += 1;
      acc.github_open += issue.github_state === "OPEN" ? 1 : 0;
      acc.by_status[issue.resolution.status] = (acc.by_status[issue.resolution.status] || 0) + 1;
      acc.by_confidence[issue.resolution.confidence] = (acc.by_confidence[issue.resolution.confidence] || 0) + 1;
      acc.by_final_disposition[issue.final_disposition.outcome] =
        (acc.by_final_disposition[issue.final_disposition.outcome] || 0) + 1;
      if (issue.final_disposition.placeholder) acc.placeholder_final_dispositions += 1;
      if (issue.resolution.objective_reached === true) acc.objective_reached_true += 1;
      else if (issue.resolution.objective_reached === false) acc.objective_reached_false += 1;
      else acc.objective_reached_unknown += 1;
      return acc;
    },
    {
      total: 0,
      unique_issue_entries: uniqueIssueNumbers.size,
      github_open: 0,
      objective_reached_true: 0,
      objective_reached_false: 0,
      objective_reached_unknown: 0,
      by_status: {},
      by_confidence: {},
      by_final_disposition: {},
      placeholder_final_dispositions: 0,
    },
  );

  const report = {
    repository: "reddb-io/reddb",
    generated_at: new Date().toISOString(),
    source_state: {
      github_issues: issuesPath,
      current_workspace_files_indexed: fileIndex.length,
      indexed_roots: roots,
      filter: "specific terms only: paths, identifiers, scoped actions, multi-word phrases; generic single SQL/common tokens excluded",
    },
    semantics: {
      code_evidence_confirmed: "At least two strong current code/test matches.",
      code_evidence_partial: "Some specific current code/test evidence, but not enough to prove every acceptance criterion.",
      covered_by_later_or_child_issue: "Parent/umbrella issue covered through child/later slice code evidence.",
      weak_or_historical_evidence_only: "Closed/done/docs/generic evidence only; no strong current code proof.",
      no_current_code_evidence_found: "No current code/test evidence found.",
      final_dispositions: {
        confirmed: "The issue has enough current evidence, or a placeholder says strong evidence exists while workflow reconciliation remains.",
        superseded: "A later or child issue is the current source of truth for the behavior.",
        reopened: "The issue needs more work or domain review before it can be release-ready.",
        split: "The issue is expected to resolve through one or more narrower follow-up issues.",
      },
    },
    summary,
    issues: reportIssues,
  };

  const codeEvidencePath = path.join(outDir, "github_issues_code_evidence_status.json");
  const originalPath = path.join(outDir, "github_issues_objective_status.json");
  fs.writeFileSync(codeEvidencePath, JSON.stringify(report, null, 2) + "\n");
  fs.writeFileSync(originalPath, JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify({ codeEvidencePath, originalPath, summary }, null, 2));
}

main();
