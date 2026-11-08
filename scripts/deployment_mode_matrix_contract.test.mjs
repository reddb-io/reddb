import assert from "node:assert/strict";
import fs from "node:fs";
import { spawnSync } from "node:child_process";
import test from "node:test";

const read = (file) => fs.readFileSync(file, "utf8");

test("deployment-mode matrix is structurally valid and evidence-backed", () => {
  const result = spawnSync(process.execPath, ["scripts/verify-deployment-mode-matrix.mjs"], {
    encoding: "utf8",
  });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /every official facade accounts for all four modes/);
});

test("CI and release both enforce the four-mode contract", () => {
  const ci = read(".github/workflows/ci.yml");
  const release = read(".github/workflows/release.yml");
  assert.match(ci, /node scripts\/verify-deployment-mode-matrix\.mjs/);
  assert.match(release, /node scripts\/verify-deployment-mode-matrix\.mjs/);
  assert.match(ci, /test\/native-engine\.e2e\.test\.mjs/);
  assert.match(ci, /tests\/test_smoke\.py/);
  assert.match(ci, /cargo test --locked -p reddb-io-ffi/);
  assert.match(ci, /C\+\+ embedded single-file journey[\s\S]*embedded_test/);
  assert.match(ci, /Zig connection routing contract[\s\S]*connect-contract/);
  assert.match(ci, /Zig embedded single-file journey[\s\S]*embedded-test/);
  assert.match(release, /Package shared embedded ABI[\s\S]*reddb_abi_version/);
});

test("the contract points at the shared native ABI", () => {
  const adr = read(".red/adr/0081-four-deployment-mode-driver-contract.md");
  const header = read("crates/reddb-ffi/include/reddb.h");
  const drivers = read("docs/clients/drivers.md");
  assert.match(adr, /serverless[\s\S]*embedded[\s\S]*server with read replicas[\s\S]*cluster/);
  assert.match(adr, /canonical durable database artifact is one `\.rdb` file/);
  assert.match(header, /reddb_open\(/);
  assert.match(header, /#define REDDB_ABI_VERSION 1u/);
  assert.match(header, /reddb_call\(/);
  assert.match(header, /reddb_invoke\(/);
  assert.match(header, /reddb_close\(/);
  assert.match(drivers, /deployment-mode-matrix\.json/);
});
