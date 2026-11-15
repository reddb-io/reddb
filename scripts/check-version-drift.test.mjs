import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const guard = join(scriptsDir, "check-version-drift.sh");

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
}

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "reddb-version-drift-"));
  const remote = join(root, "origin.git");
  const repo = join(root, "repo");

  execFileSync("git", ["init", "--bare", remote]);
  execFileSync("git", ["init", repo]);
  git(repo, "config", "user.name", "Release Test");
  git(repo, "config", "user.email", "release-test@reddb.dev");
  writeFileSync(join(repo, "fixture"), "release/1.36\n");
  git(repo, "add", "fixture");
  git(repo, "commit", "-m", "fixture");
  git(repo, "branch", "-M", "release/1.36");
  git(repo, "remote", "add", "origin", remote);
  git(repo, "push", "-u", "origin", "release/1.36");
  git(repo, "tag", "-a", "v1.36.13", "-m", "v1.36.13");
  git(repo, "tag", "-a", "v1.39.1", "-m", "v1.39.1");

  return { root, repo };
}

function runGuard(repo, overrides = {}) {
  return spawnSync("bash", [guard, "1.36.13"], {
    cwd: repo,
    encoding: "utf8",
    env: {
      ...process.env,
      REDDB_MAINTENANCE_RELEASE_BRANCH: "release/1.36",
      GITHUB_REF: "refs/tags/v1.36.13",
      ...overrides,
    },
  });
}

test("allows the exact maintenance tag at the remote release/1.36 HEAD", (t) => {
  const { root, repo } = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));

  const result = runGuard(repo);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /authorized maintenance release/);
});

test("denies a maintenance opt-in for the wrong tag", (t) => {
  const { root, repo } = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));

  const result = runGuard(repo, { GITHUB_REF: "refs/tags/v1.36.12" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /exact tag ref/);
});

test("denies a maintenance opt-in for the wrong release branch or minor", (t) => {
  const { root, repo } = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));

  const result = runGuard(repo, {
    REDDB_MAINTENANCE_RELEASE_BRANCH: "release/1.35",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /authorized only from release\/1\.36/);
});

test("denies a tag whose commit is not the remote release branch HEAD", (t) => {
  const { root, repo } = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));

  writeFileSync(join(repo, "fixture"), "local-only commit\n");
  git(repo, "add", "fixture");
  git(repo, "commit", "-m", "advance local head");
  git(repo, "tag", "-d", "v1.36.13");
  git(repo, "tag", "-a", "v1.36.13", "-m", "v1.36.13");

  const result = runGuard(repo);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /remote release\/1\.36 HEAD/);
});

test("keeps the global latest-tag guard when maintenance is not opted in", (t) => {
  const { root, repo } = fixture();
  t.after(() => rmSync(root, { recursive: true, force: true }));

  const result = runGuard(repo, {
    REDDB_MAINTENANCE_RELEASE_BRANCH: "",
    GITHUB_REF: "",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /BEHIND latest published tag v1\.39\.1/);
});
