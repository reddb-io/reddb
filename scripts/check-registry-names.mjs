import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");

const npmPublicPackages = [
  "package.json",
  "drivers/js/package.json",
  "drivers/js-client/package.json",
  "drivers/bun/package.json",
];

const npmSupportPackages = [
  "packages/internal-asset-fetcher/package.json",
  "packages/internal-bin-resolver/package.json",
  "packages/internal-version-compare/package.json",
];

const cargoPackages = [
  "Cargo.toml",
  "crates/reddb-client/Cargo.toml",
  "crates/reddb-client-connector/Cargo.toml",
  "crates/reddb-grpc-proto/Cargo.toml",
  "crates/reddb-server/Cargo.toml",
  "crates/reddb-wire/Cargo.toml",
];

function readJson(relativePath) {
  return JSON.parse(fs.readFileSync(path.join(repoRoot, relativePath), "utf8"));
}

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

function cargoName(relativePath) {
  const manifest = read(relativePath);
  const match = manifest.match(/^name\s*=\s*"([^"]+)"/m);
  assert.ok(match, `${relativePath} is missing [package].name`);
  return match[1];
}

for (const relativePath of npmPublicPackages) {
  const pkg = readJson(relativePath);
  assert.match(
    pkg.name,
    /^@reddb-io\//,
    `${relativePath} must publish under the @reddb-io npm org scope`,
  );
}

for (const relativePath of npmSupportPackages) {
  const pkg = readJson(relativePath);
  assert.notEqual(pkg.private, true, `${relativePath} must be publishable`);
  assert.match(
    pkg.name,
    /^@reddb-io\/internal-/,
    `${relativePath} must publish under @reddb-io/internal-*`,
  );
}

for (const relativePath of cargoPackages) {
  const name = cargoName(relativePath);
  assert.match(
    name,
    /^reddb-io($|-)/,
    `${relativePath} must use the reddb-io or reddb-io-* crates.io prefix`,
  );
}

console.log("registry package names are canonical");
