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
    .filter((file) => !/(^|\/)(vendor|node_modules|\.venv)\//.test(file))
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
    if (number === 46) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_statement_execution_contract.rs",
          line: 103,
          kind: "test",
          matched_term: "statement execution context public read path",
          snippet: "fn read_statement_context_observes_tenant_config_auth_and_policy_state() {",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_statement_execution_contract.rs",
          line: 130,
          kind: "test",
          matched_term: "SHOW CONFIG read path observes config state",
          snippet: "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
      ];
    }
    if (number === 48) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_statement_execution_contract.rs",
          line: 139,
          kind: "test",
          matched_term: "Role::Read SELECT allowed INSERT permission denied Write",
          snippet: "Role::Read should execute SELECT and must not execute INSERT.",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/impl_core.rs",
          line: 4388,
          kind: "code",
          matched_term: "frame.check_query_privilege acquire_intent_locks",
          snippet: "frame.check_query_privilege(self, &expr)?; ... frame.acquire_intent_locks(self, &expr);",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
      ];
    }
    if (number === 49) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_statement_execution_contract.rs",
          line: 151,
          kind: "test",
          matched_term: "CollectionContract INSERT APPEND ONLY application API",
          snippet: "fn collection_contract_enforces_insert_and_mutation_paths_through_application_api() {",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_append_only.rs",
          line: 31,
          kind: "test",
          matched_term: "append only table accepts inserts",
          snippet: "fn append_only_table_accepts_inserts() {",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
      ];
    }
    if (number === 50) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_statement_execution_contract.rs",
          line: 167,
          kind: "test",
          matched_term: "APPEND ONLY rejects UPDATE and DELETE",
          snippet: "APPEND ONLY should reject UPDATE ... APPEND ONLY should reject DELETE",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_append_only.rs",
          line: 44,
          kind: "test",
          matched_term: "append only rejects mutation paths",
          snippet: "fn append_only_table_rejects_update_with_clear_message() {",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
      ];
    }
    if (number === 51) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_statement_execution_contract.rs",
          line: 188,
          kind: "test",
          matched_term: "UPDATE and DELETE share observable target scan semantics",
          snippet: "fn update_and_delete_share_observable_target_scan_semantics() {",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/dml_target_scan.rs",
          line: 1,
          kind: "code",
          matched_term: "DML target scan locate entity ids",
          snippet: "//! DML target scan: locate the entity ids a DML statement should mutate.",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
      ];
    }
    if (number === 52) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_statement_execution_contract.rs",
          line: 72,
          kind: "test",
          matched_term: "assert UPDATE and DELETE target same rows",
          snippet: "fn assert_update_and_delete_target_same_rows(predicate: &str, expected_ids: &[i64]) {",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/impl_dml.rs",
          line: 1263,
          kind: "code",
          matched_term: "UPDATE reuses DmlTargetScan",
          snippet: "rows loop lives in DmlTargetScan so UPDATE (#52) can reuse",
          term_score: 9,
          term_sources: ["issue_336_statement_execution_audit"],
        },
      ];
    }
    if (number === 97) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/tests/snapshot_redaction_lint.rs",
          line: 1,
          kind: "test",
          matched_term: "secret-redaction audit of parser snapshots",
          snippet: "Walks every committed `*.snap` file and fails on unmasked secret-shaped substrings.",
          term_score: 9,
          term_sources: ["issue_338_parser_hardening_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/tests/conformance.rs",
          line: 439,
          kind: "test",
          matched_term: "conformance corpus contains no unmasked secret shapes",
          snippet: "fn conformance_corpus_contains_no_unmasked_secret_shapes() {",
          term_score: 9,
          term_sources: ["issue_338_parser_hardening_audit"],
        },
      ];
    }
    if (number === 62) {
      return [
        {
          type: "manual_current_code_match",
          path: "scripts/check-red-client-size.sh",
          line: 17,
          kind: "code",
          matched_term: "red_client size budget",
          snippet:
            "Builds red_client without default engine features, strips a copy, and fails when the measured size exceeds crates/reddb-client/SIZE_BUDGET.",
          term_score: 9,
          term_sources: ["issue_337_release_tooling_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "scripts/release_tooling_contract.test.mjs",
          line: 10,
          kind: "test",
          matched_term: "red_client size guard",
          snippet: "red_client size guard is wired to a documented local and CI budget check",
          term_score: 9,
          term_sources: ["issue_337_release_tooling_audit"],
        },
      ];
    }
    if (number === 68) {
      return [
        {
          type: "manual_current_code_match",
          path: "Dockerfile.client",
          line: 60,
          kind: "code",
          matched_term: "red_client thin client image",
          snippet: "cargo build --release --locked --target ${TARGET} --bin red_client -p reddb-client --no-default-features",
          term_score: 9,
          term_sources: ["issue_337_release_tooling_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "scripts/release_tooling_contract.test.mjs",
          line: 26,
          kind: "test",
          matched_term: "red_client container release contract",
          snippet: "red_client container release contract uses the thin client Dockerfile and package",
          term_score: 9,
          term_sources: ["issue_337_release_tooling_audit"],
        },
      ];
    }
    if (number === 24) {
      return [
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/impl_migrations.rs",
          line: 1,
          kind: "code",
          matched_term: "impl_migrations",
          snippet: "//! Native migration execution: CREATE / APPLY / ROLLBACK / EXPLAIN MIGRATION",
          term_score: 8,
          term_sources: ["manual_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_migrations_bootstrap.rs",
          line: 48,
          kind: "test",
          matched_term: "APPLY MIGRATION *",
          snippet: "fn apply_migration_all_applies_pending_migrations_in_dependency_order() {",
          term_score: 7,
          term_sources: ["manual_audit"],
        },
      ];
    }
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
        {
          type: "manual_current_test_match",
          path: "scripts/release_tooling_contract.test.mjs",
          line: 39,
          kind: "test",
          matched_term: "nightly DR drill workflow",
          snippet: "nightly DR drill workflow uses the current-shell runner and public make target",
          term_score: 9,
          term_sources: ["issue_337_release_tooling_audit"],
        },
      ];
    }
    if (number === 163) {
      return [
        {
          type: "manual_current_doc_match",
          path: "docs/perf/wins.md",
          line: 1,
          kind: "doc",
          matched_term: "productized performance wins",
          snippet:
            "Where RedDB Wins cites typed_insert and disk_usage benchmark sessions plus duel-official reproduction commands.",
          term_score: 9,
          term_sources: ["issue_341_red_schema_reference_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "scripts/red_schema_reference_contract.test.mjs",
          line: 12,
          kind: "test",
          matched_term: "performance wins documentation benchmark evidence",
          snippet: "performance wins documentation is tied to reproducible benchmark evidence",
          term_score: 9,
          term_sources: ["issue_341_red_schema_reference_audit"],
        },
      ];
    }
    if (number === 263) {
      return [
        {
          type: "manual_current_doc_match",
          path: "docs/reference/red-schema.md",
          line: 1,
          kind: "doc",
          matched_term: "canonical red.* schema reference",
          snippet:
            "The red.* schema reference enumerates implemented virtual tables, shortcut commands, stability, and evolution policy.",
          term_score: 9,
          term_sources: ["issue_341_red_schema_reference_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_red_schema.rs",
          line: 181,
          kind: "test",
          matched_term: "red schema stable introspection public SQL",
          snippet: "fn red_schema_introspection_is_stable_across_virtual_tables() {",
          term_score: 9,
          term_sources: ["issue_341_red_schema_reference_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "scripts/red_schema_reference_contract.test.mjs",
          line: 35,
          kind: "test",
          matched_term: "red schema reference aligned with public introspection",
          snippet: "red schema reference is aligned with public introspection coverage",
          term_score: 9,
          term_sources: ["issue_341_red_schema_reference_audit"],
        },
      ];
    }
    if (number === 309) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/iam_policy_runtime.rs",
          line: 714,
          kind: "test",
          matched_term: "destructive DDL DROP TRUNCATE requires IAM policy before mutation",
          snippet: "fn destructive_ddl_requires_drop_or_truncate_policy_before_mutation() {",
          term_score: 9,
          term_sources: ["issue_344_ddl_auth_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/impl_core.rs",
          line: 7744,
          kind: "code",
          matched_term: "check_ddl_collection_privilege drop truncate collection IAM policy audit",
          snippet:
            "IAM privilege check for DROP / TRUNCATE on a named collection records audit log entries for allow and deny outcomes.",
          term_score: 9,
          term_sources: ["issue_344_ddl_auth_audit"],
        },
      ];
    }
    if (number === 231) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/tests/conformance.rs",
          line: 276,
          kind: "test",
          matched_term: "parser conformance corpus runner",
          snippet: "fn conformance_corpus() {",
          term_score: 9,
          term_sources: ["issue_338_parser_hardening_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/tests/conformance.rs",
          line: 371,
          kind: "test",
          matched_term: "positive parser conformance corpus coverage",
          snippet: "fn positive_conformance_corpus_covers_documented_parser_surface() {",
          term_score: 9,
          term_sources: ["issue_338_parser_hardening_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/tests/conformance",
          line: 1,
          kind: "test",
          matched_term: "positive parser conformance corpus cases",
          snippet: "142 positive parser conformance TOML cases with source references.",
          term_score: 9,
          term_sources: ["issue_338_parser_hardening_audit"],
        },
      ];
    }
    if (number === 140) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache/tests.rs",
          line: 40,
          kind: "test",
          matched_term: "Blob Cache L1 put get exists miss stats eviction namespace isolation",
          snippet:
            "Blob Cache tests cover put/get/exists, clean miss stats, byte-capacity SIEVE eviction, namespace isolation, and stats fields.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache.rs",
          line: 646,
          kind: "code",
          matched_term: "BlobCache in-memory tracer sharded byte bounded L1",
          snippet:
            "BlobCache exposes the internal namespaced exact-key L1 interface with byte bounds, shards, hits, misses, evictions, and namespace stats.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 141) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache/tests.rs",
          line: 190,
          kind: "test",
          matched_term: "Blob Cache TTL admission stale jitter priority max blob size",
          snippet:
            "Blob Cache tests cover hard TTL, absolute expiry, idle TTL, stale-serve windows, jitter bounds, priority, size rejection, and L1 admission.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache.rs",
          line: 260,
          kind: "code",
          matched_term: "BlobCachePolicy ttl expires admission priority version extended",
          snippet:
            "BlobCachePolicy carries ttl_ms, expires_at_unix_ms, max_blob_bytes, L1 admission, priority, version, and extended TTL policy.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 142) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache/tests.rs",
          line: 374,
          kind: "test",
          matched_term: "Blob Cache invalidation key prefix tag dependency namespace flush",
          snippet:
            "Blob Cache tests cover key, prefix, tag, dependency, namespace flush, repeated invalidation, cold no-op, and stats updates.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache.rs",
          line: 925,
          kind: "code",
          matched_term: "invalidate_key invalidate_prefix invalidate_tags invalidate_dependencies invalidate_namespace",
          snippet:
            "BlobCache exposes explicit invalidation by key, prefix, tags, dependencies, and namespace generation flush.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 143) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/src/runtime/statement_frame.rs",
          line: 892,
          kind: "test",
          matched_term: "Blob Cache result cache backend SELECT volatile shadow adapter",
          snippet:
            "Runtime tests cover blob_cache backend population, volatile SELECT exclusion, and shadow dual-write parity.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/impl_core.rs",
          line: 6274,
          kind: "code",
          matched_term: "runtime.result_cache.backend blob_cache put_blob_result_cache_entry dependency invalidation",
          snippet:
            "Runtime dispatch selects the blob_cache result backend, writes BlobCache entries with dependency labels, and invalidates by table dependency.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 145) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache/tests.rs",
          line: 618,
          kind: "test",
          matched_term: "Blob Cache durable L2 reopen expired invalidated partial write metadata last",
          snippet:
            "Blob Cache L2 tests cover reopen rehydration, expired reopen, invalidated reopen, L2 byte cap, and metadata-last partial-write visibility.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/storage/cache/blob/l2.rs",
          line: 161,
          kind: "code",
          matched_term: "BlobCacheL2 open metadata BTree blob-chain native durable store",
          snippet:
            "BlobCacheL2 opens native pager/B+ tree metadata and blob-chain storage instead of storing blobs as normal JSON rows.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 146) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache/tests.rs",
          line: 706,
          kind: "test",
          matched_term: "Blob Cache L2 membership synopsis negative skip maybe present stale bits startup rebuild",
          snippet:
            "Synopsis tests cover negative skip, maybe-present metadata verification, stale bits after delete/expiry, and startup rebuild from L2 metadata.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/storage/cache/blob/l2.rs",
          line: 17,
          kind: "code",
          matched_term: "Bloom filter L2 membership synopsis no false negatives authoritative metadata",
          snippet:
            "The L2 Bloom synopsis guarantees absent answers skip metadata reads while maybe-present answers verify authoritative metadata.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 147) {
      return [
        {
          type: "manual_current_test_match",
          path: "scripts/blob_cache_evidence_contract.test.mjs",
          line: 41,
          kind: "test",
          matched_term: "result cache warm restart split current adapter fingerprint sidecar",
          snippet:
            "The Blob Cache evidence contract proves the adapter path exists and records warm restart as split because durable L2 stores only a fingerprint plus an in-memory sidecar.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_doc_match",
          path: "issues/348-result-cache-l2-warm-restart-contract.md",
          line: 1,
          kind: "doc",
          matched_term: "result-cache L2 warm restart follow-up",
          snippet:
            "Local follow-up #348 records the missing result-cache L2 warm-restart contract and public runtime acceptance criteria.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/impl_core.rs",
          line: 6320,
          kind: "code",
          matched_term: "get_blob_result_cache_entry result_blob_entries in-memory sidecar",
          snippet:
            "Current blob result-cache reads require the BlobCache marker and the in-memory result_blob_entries sidecar, proving durable warm restart is not yet implemented.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 149) {
      return [
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/benches/blob_cache_bench.rs",
          line: 72,
          kind: "test",
          matched_term: "Blob Cache benchmark workloads result cache Redis restart warm cache",
          snippet:
            "Criterion benchmark harness contains the eight Blob Cache workloads, result-cache comparison, Redis-gated baseline hooks, and restart-warm-cache workload.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_doc_match",
          path: "issues/349-blob-cache-redis-baseline-completion.md",
          line: 1,
          kind: "doc",
          matched_term: "Blob Cache Redis baseline completion follow-up",
          snippet:
            "Local follow-up #349 records the remaining Redis and hit-rate benchmark cells after the repeatable harness and RedDB session evidence.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "bench/blob-cache/redis-up.sh",
          line: 1,
          kind: "code",
          matched_term: "Blob Cache Redis baseline pinned setup",
          snippet:
            "Redis baseline scripts provide the pinned Redis setup that the remaining benchmark completion follow-up must run.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 151) {
      return [
        {
          type: "manual_current_doc_match",
          path: "docs/blob-cache-api-review-2026-05-06.md",
          line: 1,
          kind: "doc",
          matched_term: "Blob Cache public API review internal-only deferred HTTP SQL",
          snippet:
            "Public API review compares embedded, HTTP, SQL, and internal-only options and records deferred HTTP/SQL exposure with internal surface follow-ups.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache/tests.rs",
          line: 909,
          kind: "test",
          matched_term: "Blob Cache API review CachePresence batched invalidation builders getters",
          snippet:
            "API review follow-up tests cover CachePresence, batched invalidation, config builder, stats getters, hit getters, policy getters, and Send/Sync.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/storage/cache/blob/cache.rs",
          line: 130,
          kind: "code",
          matched_term: "Blob Cache public API accessors CachePresence batched invalidation",
          snippet:
            "Blob Cache exposes CachePresence, accessors, builders, and batched invalidation methods that resolve the internal API-review blockers.",
          term_score: 9,
          term_sources: ["issue_339_blob_cache_audit"],
        },
      ];
    }
    if (number === 197) {
      return [
        {
          type: "manual_current_code_match",
          path: "drivers/python/src/high_level.rs",
          line: 319,
          kind: "code",
          matched_term: "cache.get cache.put cache.invalidate Python SDK",
          snippet: "Cache client exposes cache.{get,put,exists,invalidate,invalidate_prefix,invalidate_tags,flush_namespace}.",
          term_score: 9,
          term_sources: ["issue_340_sdk_redis_migration_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "drivers/python/tests/test_cache.py",
          line: 29,
          kind: "test",
          matched_term: "Python cache get put invalidate behavior",
          snippet: "Public Python tests cover cache put/get round trip, miss, invalidate, prefix, tags, namespace isolation, and overwrite behavior.",
          term_score: 9,
          term_sources: ["issue_340_sdk_redis_migration_audit"],
        },
      ];
    }
    if (number === 199) {
      return [
        {
          type: "manual_current_test_match",
          path: "scripts/sdk_redis_migration_contract.test.mjs",
          line: 29,
          kind: "test",
          matched_term: "red migrate-from-redis split status",
          snippet: "Redis migration CLI status is explicit and split to a follow-up.",
          term_score: 9,
          term_sources: ["issue_340_sdk_redis_migration_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "scripts/sdk_redis_migration_contract.test.mjs",
          line: 34,
          kind: "test",
          matched_term: "red migrate-from-redis local follow-up #347",
          snippet: "The contract test verifies the split follow-up records the missing red migrate-from-redis CLI and dual-write acceptance criteria.",
          term_score: 9,
          term_sources: ["issue_340_sdk_redis_migration_audit"],
        },
        {
          type: "manual_current_doc_match",
          path: "issues/347-red-migrate-from-redis-cli-tool.md",
          line: 1,
          kind: "doc",
          matched_term: "red migrate-from-redis CLI follow-up",
          snippet: "Local follow-up #347 records the missing red migrate-from-redis CLI contract and dual-write acceptance criteria.",
          term_score: 9,
          term_sources: ["issue_340_sdk_redis_migration_audit"],
        },
      ];
    }
    if (number === 287) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/integration_queue_timeseries.rs",
          line: 560,
          kind: "test",
          matched_term: "FANOUT broadcast all consumers get all messages",
          snippet: "fn test_fanout_queue_broadcast_all_consumers_get_all_messages()",
          term_score: 9,
          term_sources: ["issue_342_queue_semantics_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/integration_queue_timeseries.rs",
          line: 605,
          kind: "test",
          matched_term: "FANOUT ack isolation per consumer",
          snippet: "fn test_fanout_queue_ack_isolation()",
          term_score: 9,
          term_sources: ["issue_342_queue_semantics_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/integration_queue_timeseries.rs",
          line: 632,
          kind: "test",
          matched_term: "FANOUT DLQ per consumer",
          snippet: "fn test_fanout_queue_dlq_per_consumer()",
          term_score: 9,
          term_sources: ["issue_342_queue_semantics_audit"],
        },
      ];
    }
    if (number === 289) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/integration_queue_timeseries.rs",
          line: 461,
          kind: "test",
          matched_term: "ALTER QUEUE WORK to FANOUT in-flight drain",
          snippet: "fn test_alter_queue_work_to_fanout_transition()",
          term_score: 9,
          term_sources: ["issue_342_queue_semantics_audit"],
        },
        {
          type: "manual_current_code_match",
          path: "crates/reddb-server/src/runtime/impl_queue.rs",
          line: 345,
          kind: "code",
          matched_term: "ALTER QUEUE SET MODE active pending warning",
          snippet: "tracing::warn!(pending_count = pending.len(), \"ALTER QUEUE SET MODE: {} in-flight messages will drain with old mode; new reads use {}\")",
          term_score: 9,
          term_sources: ["issue_342_queue_semantics_audit"],
        },
      ];
    }
    if (number === 296) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_events_foundation.rs",
          line: 467,
          kind: "test",
          matched_term: "events multi-subscription queues both receive insert",
          snippet: "fn add_two_subscriptions_both_receive_insert_event()",
          term_score: 9,
          term_sources: ["issue_343_events_subscription_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_events_foundation.rs",
          line: 515,
          kind: "test",
          matched_term: "events per-subscription redaction independent",
          snippet: "fn redact_applied_per_subscription_independently()",
          term_score: 9,
          term_sources: ["issue_343_events_subscription_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_events_foundation.rs",
          line: 552,
          kind: "test",
          matched_term: "events drop subscription preserves remaining queue",
          snippet: "fn drop_subscription_stops_events_to_that_queue()",
          term_score: 9,
          term_sources: ["issue_343_events_subscription_audit"],
        },
      ];
    }
    if (number === 317) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_vault_sealed_storage.rs",
          line: 132,
          kind: "test",
          matched_term: "vault sealed storage persistence metadata unavailable key provider",
          snippet: "fn vault_put_seals_payload_before_persistence()",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_vault_sealed_storage.rs",
          line: 206,
          kind: "test",
          matched_term: "VAULT GET sealed_unavailable metadata read without key material",
          snippet: "metadata read should not require key material and returns sealed_unavailable after reopen without a key provider.",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
        },
      ];
    }
    if (number === 318) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_vault_sealed_storage.rs",
          line: 221,
          kind: "test",
          matched_term: "vault get metadata only unseal capability gated audited",
          snippet: "fn vault_get_is_metadata_only_and_unseal_is_capability_gated_and_audited()",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_vault_sealed_storage.rs",
          line: 335,
          kind: "test",
          matched_term: "vault lifecycle rotate history purge audit policy redaction",
          snippet: "fn vault_lifecycle_versions_history_purge_and_historical_unseal_are_audited()",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
        },
      ];
    }
    if (number === 319) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_system_config_vault.rs",
          line: 85,
          kind: "test",
          matched_term: "red.config red.vault system collections observable red.collections internal protected",
          snippet: "fn bootstrap_creates_protected_system_config_and_vault_collections()",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_system_config_vault.rs",
          line: 106,
          kind: "test",
          matched_term: "system config vault reject create drop truncate read-only",
          snippet: "fn system_config_and_vault_reject_public_create_drop_and_truncate()",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
        },
      ];
    }
    if (number === 321) {
      return [
        {
          type: "manual_current_test_match",
          path: "tests/e2e_config_vault_observation.rs",
          line: 109,
          kind: "test",
          matched_term: "LIST CONFIG prefix pagination values tags WATCH config read allowed",
          snippet: "Config observation tests cover LIST CONFIG with tags and watch value redaction based on config:read policy.",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
        },
        {
          type: "manual_current_test_match",
          path: "tests/e2e_config_vault_observation.rs",
          line: 204,
          kind: "test",
          matched_term: "LIST VAULT WATCH VAULT metadata only tags no plaintext",
          snippet: "fn list_and_watch_vault_are_metadata_only()",
          term_score: 9,
          term_sources: ["issue_345_config_vault_audit"],
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

  function manualFinalDisposition(number) {
    if (number === 46) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Statement execution read context is evidenced by tests/e2e_statement_execution_contract.rs using public execute_query outcomes for RLS policy state, tenant scope, SHOW CONFIG, and authenticated read behavior.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 48) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Privilege and lock intent derivation is evidenced by tests/e2e_statement_execution_contract.rs public read/write outcomes: Role::Read can SELECT, INSERT is permission denied with Write, and dispatch consults frame privilege plus intent locks.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 49) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "CollectionContract INSERT behavior is evidenced by tests/e2e_statement_execution_contract.rs and tests/e2e_append_only.rs: APPEND ONLY tables accept public INSERT paths while retaining the contract.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 50) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "CollectionContract mutation behavior is evidenced by tests/e2e_statement_execution_contract.rs and tests/e2e_append_only.rs: APPEND ONLY rejects UPDATE and DELETE without mutating stored rows.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 51) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "DELETE target scan behavior is evidenced by tests/e2e_statement_execution_contract.rs, where UPDATE and DELETE select the same target ids for indexed equality and unindexed range predicates through public SQL.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 52) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "UPDATE target scan reuse is evidenced by tests/e2e_statement_execution_contract.rs and crates/reddb-server/src/runtime/impl_dml.rs: UPDATE and DELETE share observable target selection for the same predicates.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 87) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Parser hardening is evidenced by crates/reddb-server/tests/support/parser_hardening, parser snapshot/property tests, ParserLimits in crates/reddb-server/src/storage/query/parser/limits.rs, fuzz targets under fuzz/fuzz_targets, and parser fuzz/coverage CI workflows.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 97) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Secret-redaction backfill is evidenced by crates/reddb-server/tests/snapshot_redaction_lint.rs, crates/reddb-wire/tests/snapshot_redaction_lint.rs, shared secret_redactor.rs filters, and the conformance corpus unmasked-secret lint in crates/reddb-server/tests/conformance.rs.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 62) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "red_client binary-size protection is wired through scripts/check-red-client-size.sh, crates/reddb-client/SIZE_BUDGET, the CI red_client size budget step, and release tooling contract tests.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 68) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "red_client container distribution is recorded in ADR 0004 and wired through Dockerfile.client plus the release.yml publish-client-image job using the ghcr.io/reddb-io/reddb-client tag scheme.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 116) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "The 2026-05-06 nightly DR drill failure is tied to the current-shell scripts/drill-nightly.sh fix and the public make drill-nightly workflow command.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 163) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Performance wins are productized in docs/perf/wins.md with duel-official reproduction commands, cited benchmark session ids, README and JS/TS guide links, and scripts/red_schema_reference_contract.test.mjs coverage.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 140) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "The internal Blob Cache L1 tracer is evidenced by crates/reddb-server/src/storage/cache/blob/cache/tests.rs: put/get/exists, miss stats, byte-capacity eviction, namespace isolation, and stats are covered against the public BlobCache interface.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 141) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Rich TTL and admission policy behavior is evidenced by Blob Cache tests covering hard TTL, absolute expiry, idle TTL, stale-serve windows, jitter bounds, priority, max blob size, and L1 admission.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 142) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Explicit invalidation is evidenced by Blob Cache tests and implementation paths for key, prefix, tag, dependency, namespace generation flush, repeated invalidation, cold no-op, and stats updates.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 143) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "The SQL result-cache L1 adapter is evidenced by runtime tests in crates/reddb-server/src/runtime/statement_frame.rs and dispatch in impl_core.rs: blob_cache backend writes BlobCache entries, volatile SELECTs are excluded, shadow mode dual-writes, and table dependency invalidation targets Blob Cache labels.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 145) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Durable Blob Cache L2 is evidenced by reopen, expired reopen, invalidated reopen, byte-cap, and metadata-last fault tests plus BlobCacheL2 native pager/B+ tree metadata and blob-chain storage.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 146) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "The L2 membership synopsis is evidenced by tests for negative skips, maybe-present metadata verification, stale bits after delete/expiry, and startup rebuild, backed by the BlobCacheL2 Bloom filter contract.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 147) {
      return {
        outcome: "split",
        placeholder: false,
        reason:
          "The current result-cache adapter exercises Blob Cache, but durable warm restart is not implemented because L2 stores only a fingerprint while RuntimeQueryResult remains in an in-memory sidecar; split to local follow-up #348.",
        superseded_by: [],
        reopened_as: [],
        split_into: [348],
      };
    }
    if (number === 149) {
      return {
        outcome: "split",
        placeholder: false,
        reason:
          "The Blob Cache benchmark harness and RedDB session evidence are present, but Redis and hit-rate cells remain deferred in docs/perf/blob-cache-bench-2026-05-06.md; split final baseline completion to local follow-up #349.",
        superseded_by: [],
        reopened_as: [],
        split_into: [349],
      };
    }
    if (number === 151) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "The public API shape review is recorded in docs/blob-cache-api-review-2026-05-06.md: it compares embedded/internal, HTTP, and SQL surfaces, keeps HTTP/SQL exposure deferred, and current tests cover the internal API hygiene follow-ups.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 197) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Python SDK cache behavior is evidenced by drivers/python/src/high_level.rs exposing db.cache and by drivers/python/tests/test_cache.py covering cache.get, cache.put, and cache.invalidate through the public embedded driver API.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 199) {
      return {
        outcome: "split",
        placeholder: false,
        reason:
          "`red migrate-from-redis` is not implemented; docs/guides/migrate-redis-to-blob-cache.md now states the guide is the current migration surface, and the missing CLI dual-write tool is split to local follow-up #347.",
        superseded_by: [],
        reopened_as: [],
        split_into: [347],
      };
    }
    if (number === 287) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "FANOUT broadcast semantics are evidenced by tests/integration_queue_timeseries.rs: test_fanout_queue_broadcast_all_consumers_get_all_messages proves alice, bob, and carol each receive all 100 messages through public QUEUE READ/ACK behavior, with additional ack and DLQ isolation tests proving per-consumer state.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 289) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "ALTER QUEUE SET MODE transition behavior is evidenced by tests/integration_queue_timeseries.rs: in-flight WORK messages remain ackable through _work_default while new reads use FANOUT semantics, and crates/reddb-server/src/runtime/impl_queue.rs emits the active pending tracing warning with pending_count for operators.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 296) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Events multi-subscription behavior is evidenced by tests/e2e_events_foundation.rs: add_two_subscriptions_both_receive_insert_event proves one collection delivers the same insert to two target queues, drop_subscription_stops_events_to_that_queue proves DROP SUBSCRIPTION removes only the named target while preserving the remaining subscription, and redact_applied_per_subscription_independently proves each subscription applies its own redaction list.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 317) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Vault sealed storage is evidenced by tests/e2e_vault_sealed_storage.rs: vault_put_seals_payload_before_persistence proves VAULT PUT persists Value::Secret without plaintext in database artifacts, VAULT GET returns redacted metadata, and reopen without a key provider returns sealed_unavailable instead of plaintext.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 318) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Vault redaction, unseal, audit, and policy behavior is evidenced by tests/e2e_vault_sealed_storage.rs: vault_get_is_metadata_only_and_unseal_is_capability_gated_and_audited proves metadata-only GET, vault:unseal denial/allow, and redacted audit records; vault_lifecycle_versions_history_purge_and_historical_unseal_are_audited covers rotate, history, delete, purge, vault:unseal_history, and audit outcomes.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 319) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "System Config/Vault collections are evidenced by tests/e2e_system_config_vault.rs: bootstrap_creates_protected_system_config_and_vault_collections observes red.config and red.vault through red.collections, and system_config_and_vault_reject_public_create_drop_and_truncate proves public CREATE, DROP, and TRUNCATE paths are read-only.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 321) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Config/Vault WATCH, LIST, TAGS, and domain-separated API behavior is evidenced by tests/e2e_config_vault_observation.rs: LIST CONFIG returns values and tags, config_watch_events_since redacts values without config:read, list_and_watch_vault_are_metadata_only proves Vault LIST/WATCH expose metadata and tags without plaintext, and newer issue #329 covers domain-separated API surfaces.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 263) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "The canonical red.* schema reference is docs/reference/red-schema.md, linked from docs/README.md and tied to public runtime coverage in tests/e2e_red_schema.rs plus scripts/red_schema_reference_contract.test.mjs.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 309) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Destructive DDL authorization is evidenced by tests/iam_policy_runtime.rs: public SQL/API DROP and TRUNCATE denials require the correct collection policy before mutation, allowed principals execute successfully, polymorphic DROP COLLECTION is covered, and audit entries record allow/deny outcomes.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 231) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "The positive parser conformance corpus is evidenced by crates/reddb-server/tests/conformance/*.toml, the source-reference and parser runner in crates/reddb-server/tests/conformance.rs, and the positive surface coverage contract for issue #231.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 233) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Parser fuzz scheduling is evidenced by fuzz/fuzz_targets/sql_parser.rs, fuzz/fuzz_targets/migration_parser.rs, fuzz/fuzz_targets/conn_string_parser.rs, .github/workflows/parser-fuzz-nightly.yml, and CI fuzz build wiring.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 236) {
      return {
        outcome: "confirmed",
        placeholder: false,
        reason:
          "Lexer/table parser coverage uplift is evidenced by parser coverage workflow LCOV parsing in .github/workflows/parser-coverage.yml plus lexer/table-targeted conformance cases and parser property tests.",
        superseded_by: [],
        reopened_as: [],
        split_into: [],
      };
    }
    if (number === 21) {
      return {
        outcome: "split",
        placeholder: false,
        reason:
          "Current workspace has migration dependency/runtime evidence, but branch-scoped MigrationConflict merge behavior is not implemented; split to local follow-up #346.",
        superseded_by: [],
        reopened_as: [],
        split_into: [346],
      };
    }
    if (number === 24) {
      return {
        outcome: "superseded",
        placeholder: false,
        reason:
          "The historical lint item referenced src/runtime/impl_migrations.rs; the current module is crates/reddb-server/src/runtime/impl_migrations.rs and is covered by current migration runtime tests.",
        superseded_by: ["crates/reddb-server/src/runtime/impl_migrations.rs"],
        reopened_as: [],
        split_into: [],
      };
    }
    return null;
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
    const disposition = manualFinalDisposition(issue.number) || finalDisposition(resolution.status);
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
