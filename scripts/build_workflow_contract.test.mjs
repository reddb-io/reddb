import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

const root = path.resolve(import.meta.dirname, "..");
const pin = fs.readFileSync(path.join(root, "rust-toolchain.toml"), "utf8")
  .match(/^channel\s*=\s*"([^"]+)"/m)[1];

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "reddb-build-contract-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  fs.mkdirSync(path.join(dir, "scripts"));
  for (const file of ["cargo-fast.sh", "test-fast.sh"]) {
    fs.copyFileSync(path.join(root, "scripts", file), path.join(dir, "scripts", file));
    fs.chmodSync(path.join(dir, "scripts", file), 0o755);
  }
  fs.copyFileSync(path.join(root, "rust-toolchain.toml"), path.join(dir, "rust-toolchain.toml"));
  const cargo = path.join(dir, "cargo");
  const log = path.join(dir, "calls.jsonl");
  fs.writeFileSync(cargo, `#!${process.execPath}
const fs = require("node:fs");
const args = process.argv.slice(2);
const call = { args, start: Date.now(), toolchain: process.env.RUSTUP_TOOLCHAIN,
  jobs: process.env.CARGO_BUILD_JOBS, incremental: process.env.CARGO_INCREMENTAL,
  testThreads: process.env.RUST_TEST_THREADS, nextestThreads: process.env.NEXTEST_TEST_THREADS,
  target: process.env.CARGO_TARGET_DIR };
if (process.env.FAKE_HOLD_MS) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, Number(process.env.FAKE_HOLD_MS));
call.end = Date.now();
fs.appendFileSync(process.env.FAKE_CARGO_LOG, JSON.stringify(call) + "\\n");
process.exit(Number(process.env.FAKE_EXIT || 0));
`);
  fs.chmodSync(cargo, 0o755);
  const rustc = path.join(dir, "rustc");
  fs.writeFileSync(rustc, "#!/usr/bin/env bash\necho 'host: x86_64-unknown-linux-gnu'\n");
  fs.chmodSync(rustc, 0o755);
  const env = {
    ...process.env,
    REDDB_CARGO_BIN: cargo,
    RUSTC: rustc,
    XDG_RUNTIME_DIR: dir,
    CARGO_TARGET_DIR: path.join(dir, "target"),
    FAKE_CARGO_LOG: log,
    REDB_USE_SCCACHE: "0",
  };
  for (const key of ["CARGO_BUILD_JOBS", "CARGO_INCREMENTAL", "RUST_TEST_THREADS", "NEXTEST_TEST_THREADS", "REDDB_RUST_TOOLCHAIN",
    "REDDB_CARGO_LOCK", "REDDB_FAST_SHARED_TARGET", "REDDB_FAST_TARGET_DIR",
    "REDDB_FAST_TESTS", "REDDB_FAST_EXTRA_TESTS", "REDDB_FAST_VERBOSE"]) delete env[key];
  return { dir, env, log };
}

function run(f, script, args = [], extraEnv = {}) {
  const result = spawnSync("bash", [path.join(f.dir, "scripts", script), ...args], {
    cwd: f.dir, env: { ...f.env, ...extraEnv }, encoding: "utf8", timeout: 10_000,
  });
  assert.equal(result.status, 0, result.stderr);
  return fs.readFileSync(f.log, "utf8").trim().split("\n").map(JSON.parse);
}

test("wrapper respects profile incremental settings and defaults to the project pin and two jobs", (t) => {
  const f = fixture(t);
  const [call] = run(f, "cargo-fast.sh", ["build", "--profile", "release-static"], {
    RUSTUP_TOOLCHAIN: "host-latest",
  });
  assert.equal(call.toolchain, pin);
  assert.equal(call.jobs, "2");
  assert.equal(call.testThreads, "2");
  assert.equal(call.nextestThreads, "2");
  assert.equal(call.incremental, undefined);
});

test("explicit compiler, job and incremental choices remain available", (t) => {
  const f = fixture(t);
  const [call] = run(f, "cargo-fast.sh", ["+nightly", "check", "--jobs", "1"], {
    REDDB_RUST_TOOLCHAIN: "nightly",
    CARGO_BUILD_JOBS: "4",
    CARGO_INCREMENTAL: "0",
    RUST_TEST_THREADS: "3",
    NEXTEST_TEST_THREADS: "4",
  });
  assert.equal(call.toolchain, "nightly");
  assert.equal(call.jobs, "4");
  assert.equal(call.incremental, "0");
  assert.equal(call.testThreads, "3");
  assert.equal(call.nextestThreads, "4");
  assert.deepEqual(call.args, ["+nightly", "check", "--jobs", "1"]);
});

test("fast lane reuses its target, includes engine unit tests and builds the smoke client", (t) => {
  const f = fixture(t);
  const calls = run(f, "test-fast.sh");
  assert.equal(calls.length, 17);
  assert(calls.every((call) => call.target === f.env.CARGO_TARGET_DIR));
  assert(calls[0].args.includes("--workspace"));
  assert(calls[0].args.includes("--lib"));
  assert(calls[0].args.includes("--bins"));
  assert.deepEqual(calls[1].args, ["build", "--quiet", "--locked", "-p", "reddb-io-client", "--no-default-features", "--bin", "red_client"]);
  for (const call of calls.slice(2)) {
    const index = call.args.indexOf("--test");
    assert.match(call.args[index + 1], /^grouped_/);
    assert.match(call.args[index + 2], /::$/);
  }
});

test("explicit fast target and legacy target-only selection are preserved", (t) => {
  const f = fixture(t);
  const target = path.join(f.dir, "custom-target");
  const calls = run(f, "test-fast.sh", [], {
    REDDB_FAST_TARGET_DIR: target,
    REDDB_FAST_TESTS: "grouped_sql_core",
  });
  assert(calls.every((call) => call.target === target));
  assert.deepEqual(calls[2].args, ["test", "--quiet", "--locked", "--test", "grouped_sql_core"]);
});

test("an isolated fast target remains opt-in", (t) => {
  const f = fixture(t);
  const calls = run(f, "test-fast.sh", [], {
    REDDB_FAST_SHARED_TARGET: "0", REDDB_FAST_TESTS: "grouped_sql_core",
  });
  assert(calls.every((call) => call.target === path.join(f.env.CARGO_TARGET_DIR, "test-fast")));
});

test("independent target directories share one host build lease", async (t) => {
  if (spawnSync("flock", ["--version"]).status !== 0) return t.skip("flock unavailable");
  const f = fixture(t);
  const start = (target) => new Promise((resolve, reject) => {
    const child = spawn("bash", [path.join(f.dir, "scripts/cargo-fast.sh"), "check"], {
      cwd: f.dir,
      env: { ...f.env, CARGO_TARGET_DIR: target, FAKE_HOLD_MS: "250" },
      stdio: "ignore",
    });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`exit ${code}`)));
  });
  await Promise.all([start(path.join(f.dir, "worktree-a")), start(path.join(f.dir, "worktree-b"))]);
  const calls = fs.readFileSync(f.log, "utf8").trim().split("\n").map(JSON.parse)
    .sort((a, b) => a.start - b.start);
  assert.equal(calls.length, 2);
  assert(calls[1].start >= calls[0].end, "Cargo processes overlapped across targets");
});

test("Cargo failures propagate through the lease", (t) => {
  const f = fixture(t);
  const result = spawnSync("bash", [path.join(f.dir, "scripts/cargo-fast.sh"), "check"], {
    cwd: f.dir, env: { ...f.env, FAKE_EXIT: "42" }, encoding: "utf8",
  });
  assert.equal(result.status, 42);
});

test("cargo run arguments do not accidentally acquire the build lease", async (t) => {
  if (spawnSync("flock", ["--version"]).status !== 0) return t.skip("flock unavailable");
  const f = fixture(t);
  const lease = path.join(f.dir, `reddb-cargo-${process.getuid()}.lock`);
  const holder = spawn("flock", ["--close", lease, "bash", "-c", "echo locked; sleep 2"]);
  t.after(() => holder.kill());
  await new Promise((resolve, reject) => {
    holder.stdout.once("data", resolve);
    holder.once("error", reject);
  });
  const result = spawnSync("bash", [path.join(f.dir, "scripts/cargo-fast.sh"), "run", "--", "test"], {
    cwd: f.dir, env: f.env, encoding: "utf8", timeout: 1000,
  });
  assert.equal(result.status, 0, "long-lived run would wait for/hold the build lease");
});

// Inspect the declared harness graph without compiling the engine. This catches
// nonexistent curated targets and double compilation of test-bearing source files.
test("curated modules exist and test files have one harness owner", () => {
  const manifest = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
  const targets = new Map([...manifest.matchAll(/\[\[test\]\]\s*name\s*=\s*"([^"]+)"\s*path\s*=\s*"([^"]+)"/g)]
    .map((hit) => [hit[1], path.join(root, hit[2])]));
  const owners = new Map();
  const modules = new Map();
  const visit = (file, owner, seen) => {
    if (seen.has(file)) return;
    seen.add(file);
    const source = fs.readFileSync(file, "utf8");
    if (/#\[(?:tokio::)?test(?:\s*\(|\s*\])/.test(source)) {
      const previous = owners.get(file);
      assert(!previous || previous === owner, `${file} included by ${previous} and ${owner}`);
      owners.set(file, owner);
    }
    for (const hit of source.matchAll(/#\[path\s*=\s*"([^"]+)"\]\s*mod\s+(\w+);/g)) {
      const child = path.resolve(path.dirname(file), hit[1]);
      modules.get(owner).add(hit[2]);
      visit(child, owner, seen);
    }
  };
  for (const [owner, file] of targets) {
    modules.set(owner, new Set());
    visit(file, owner, new Set());
  }
  const runner = fs.readFileSync(path.join(root, "scripts/test-fast.sh"), "utf8");
  const curated = runner.match(/FAST_TESTS=\(([\s\S]*?)\)/)[1].trim().split(/\s+/);
  for (const entry of curated) {
    const [target, module] = entry.split(":");
    assert(targets.has(target), `unknown target ${target}`);
    assert(modules.get(target).has(module), `missing ${module} in ${target}`);
  }
});
