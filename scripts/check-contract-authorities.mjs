#!/usr/bin/env node
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const manifestPath = "testdata/conformance/contract-authorities.json";
const readmePath = "testdata/conformance/README.md";
const indexDir = "testdata/conformance";
const allowedIndexFiles = new Set([manifestPath, readmePath]);

function absolute(relativePath) {
  return path.join(repoRoot, relativePath);
}

function readJson(relativePath) {
  return JSON.parse(fs.readFileSync(absolute(relativePath), "utf8"));
}

function walkFiles(relativeDir) {
  const root = absolute(relativeDir);
  if (!fs.existsSync(root)) return [];
  const out = [];
  const stack = [relativeDir];
  while (stack.length > 0) {
    const dir = stack.pop();
    for (const entry of fs.readdirSync(absolute(dir), { withFileTypes: true })) {
      const rel = path.posix.join(dir, entry.name);
      if (entry.isDirectory()) {
        stack.push(rel);
      } else {
        out.push(rel);
      }
    }
  }
  return out.sort();
}

function assertSafeRelativePath(label, relativePath) {
  assert.equal(
    path.isAbsolute(relativePath),
    false,
    `${label} must be repository-relative, got absolute path ${relativePath}`,
  );
  const normalized = path.posix.normalize(relativePath);
  assert.equal(
    normalized,
    relativePath,
    `${label} must already be normalized, got ${relativePath}`,
  );
  assert.equal(
    relativePath.startsWith("../"),
    false,
    `${label} must not escape the repository: ${relativePath}`,
  );
}

const manifest = readJson(manifestPath);
assert.equal(manifest.version, 1, `${manifestPath} must use version 1`);
assert.ok(Array.isArray(manifest.authorities), `${manifestPath} must list authorities`);
assert.ok(manifest.authorities.length > 0, `${manifestPath} must not be empty`);

const seenIds = new Set();
const seenPaths = new Set();
const readme = fs.readFileSync(absolute(readmePath), "utf8");

for (const authority of manifest.authorities) {
  assert.match(
    authority.id,
    /^[a-z0-9]+(?:-[a-z0-9]+)*$/,
    `authority id must be kebab-case: ${authority.id}`,
  );
  assert.equal(seenIds.has(authority.id), false, `duplicate authority id: ${authority.id}`);
  seenIds.add(authority.id);

  assert.ok(
    authority.type === "file" || authority.type === "directory",
    `${authority.id}: type must be file or directory`,
  );
  assertSafeRelativePath(`${authority.id}.path`, authority.path);
  assert.equal(
    seenPaths.has(authority.path),
    false,
    `${authority.id}: duplicate authority path ${authority.path}`,
  );
  seenPaths.add(authority.path);

  const stat = fs.statSync(absolute(authority.path), {
    throwIfNoEntry: false,
  });
  assert.ok(stat, `${authority.id}: authority path does not exist: ${authority.path}`);
  if (authority.type === "file") {
    assert.ok(stat.isFile(), `${authority.id}: expected file: ${authority.path}`);
  } else {
    assert.ok(stat.isDirectory(), `${authority.id}: expected directory: ${authority.path}`);
  }
  assert.ok(
    typeof authority.owns === "string" && authority.owns.trim().length > 0,
    `${authority.id}: owns must describe the contract`,
  );
  assert.ok(
    readme.includes(authority.path),
    `${readmePath} must document authority path ${authority.path}`,
  );
}

const allowedConformanceRoots = manifest.authorities
  .filter((authority) => authority.path.startsWith(`${indexDir}/`))
  .map((authority) => ({
    path: authority.path,
    type: authority.type,
  }));

for (const file of walkFiles(indexDir)) {
  const listedAuthority = allowedConformanceRoots.some((authority) => {
    if (authority.type === "file") return file === authority.path;
    return file === authority.path || file.startsWith(`${authority.path}/`);
  });
  assert.ok(
    allowedIndexFiles.has(file) || listedAuthority,
    `${indexDir} file is not listed as a contract authority: ${file}`,
  );
}

console.log(`contract authorities are canonical (${manifest.authorities.length} authorities)`);
