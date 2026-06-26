import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");

const npmPublicPackages = [
  "package.json",
  "drivers/js/package.json",
  "drivers/js-client/package.json",
  "drivers/bun/package.json",
  "packages/mcp/package.json",
];

const npmPrivateWorkspacePackages = [
  "packages/internal-asset-fetcher/package.json",
  "packages/internal-bin-resolver/package.json",
  "packages/internal-version-compare/package.json",
];

function cargoManifestsIn(relativeDir) {
  const absoluteDir = path.join(repoRoot, relativeDir);
  if (!fs.existsSync(absoluteDir)) {
    return [];
  }
  return fs
    .readdirSync(absoluteDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => `${relativeDir}/${entry.name}/Cargo.toml`)
    .filter((relativePath) => fs.existsSync(path.join(repoRoot, relativePath)))
    .sort();
}

const cargoPackages = [
  "Cargo.toml",
  ...cargoManifestsIn("crates"),
  ...cargoManifestsIn("drivers"),
];

function readJson(relativePath) {
  return JSON.parse(fs.readFileSync(path.join(repoRoot, relativePath), "utf8"));
}

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

function cargoPackageSection(relativePath) {
  const manifest = read(relativePath);
  const packageHeader = manifest.search(/^\[package\]\s*$/m);
  assert.notEqual(packageHeader, -1, `${relativePath} is missing a [package] section`);
  const afterPackageHeader = manifest.slice(packageHeader).split("\n").slice(1).join("\n");
  const nextSection = afterPackageHeader.search(/^\[/m);
  return nextSection === -1 ? afterPackageHeader : afterPackageHeader.slice(0, nextSection);
}

function cargoName(relativePath) {
  const packageSection = cargoPackageSection(relativePath);
  const match = packageSection.match(/^name\s*=\s*"([^"]+)"/m);
  assert.ok(match, `${relativePath} is missing [package].name`);
  return match[1];
}

function cargoPublishDisabled(relativePath) {
  return /^publish\s*=\s*false\s*$/m.test(cargoPackageSection(relativePath));
}

for (const relativePath of npmPublicPackages) {
  const pkg = readJson(relativePath);
  assert.match(
    pkg.name,
    /^@reddb-io\//,
    `${relativePath} must publish under the @reddb-io npm org scope`,
  );
}

for (const relativePath of npmPrivateWorkspacePackages) {
  const pkg = readJson(relativePath);
  assert.equal(pkg.private, true, `${relativePath} must stay private`);
  assert.match(
    pkg.name,
    /^@reddb-io\/internal-/,
    `${relativePath} must use @reddb-io/internal-*`,
  );
}

for (const relativePath of cargoPackages) {
  if (cargoPublishDisabled(relativePath)) {
    continue;
  }
  const name = cargoName(relativePath);
  assert.match(
    name,
    /^reddb-io($|-)/,
    `${relativePath} must use the reddb-io or reddb-io-* crates.io prefix`,
  );
}

console.log("registry package names are canonical");
