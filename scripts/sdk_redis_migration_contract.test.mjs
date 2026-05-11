import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function readRepoFile(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("Python cache public API has get put invalidate behavior coverage", () => {
  const source = readRepoFile("drivers/python/src/high_level.rs");
  const tests = readRepoFile("drivers/python/tests/test_cache.py");

  assert.match(source, /Cache client[\s\S]*cache\.\{get,put,exists,invalidate,/);
  assert.match(source, /fn get<'py>\(&self, py: Python<'py>, namespace: &str, key: &str\)/);
  assert.match(source, /fn put\(/);
  assert.match(source, /fn invalidate\(&self, namespace: &str, key: &str\)/);

  assert.match(tests, /def test_put_and_get_round_trip\(\):/);
  assert.match(tests, /db\.cache\.put\("ns", "k1", b"hello"\)/);
  assert.match(tests, /assert result == b"hello"/);
  assert.match(tests, /def test_get_miss_returns_none\(\):/);
  assert.match(tests, /def test_invalidate_removes_entry\(\):/);
  assert.match(tests, /assert db\.cache\.get\("ns", "del"\) is None/);
});

test("Redis migration CLI status is explicit and split to a follow-up", () => {
  const guide = readRepoFile("docs/guides/migrate-redis-to-blob-cache.md");
  const followUp = readRepoFile("issues/347-red-migrate-from-redis-cli-tool.md");

  assert.match(guide, /`red migrate-from-redis` is not implemented/);
  assert.match(guide, /local follow-up #347/);
  assert.match(followUp, /# red migrate-from-redis CLI tool/);
  assert.match(followUp, /GitHub issue number: #347/);
  assert.match(followUp, /dual-write/);
});
