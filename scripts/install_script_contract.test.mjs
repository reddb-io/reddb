import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");
const installer = path.join(repoRoot, "install.sh");

function sha256Hex(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function writeExecutable(file, body) {
  fs.writeFileSync(file, body, "utf8");
  fs.chmodSync(file, 0o755);
}

function makeFixture(t, { files, checksums }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "reddb-install-test-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));

  const releaseDir = path.join(dir, "release");
  const binDir = path.join(dir, "bin");
  const installDir = path.join(dir, "install");
  const tmpDir = path.join(dir, "tmp");
  fs.mkdirSync(releaseDir);
  fs.mkdirSync(binDir);
  fs.mkdirSync(installDir);
  fs.mkdirSync(tmpDir);

  for (const [name, contents] of Object.entries(files)) {
    fs.writeFileSync(path.join(releaseDir, name), contents);
  }
  fs.writeFileSync(path.join(releaseDir, "SHA256SUMS"), checksums, "utf8");

  writeExecutable(
    path.join(binDir, "curl"),
    `#!/usr/bin/env bash
set -euo pipefail
output=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o)
      output="$2"
      shift 2
      ;;
    -*)
      shift
      ;;
    *)
      url="$1"
      shift
      ;;
  esac
done

if [ -z "$url" ]; then
  echo "curl fixture: missing url" >&2
  exit 2
fi

payload=""
case "$url" in
  https://api.github.com/repos/reddb-io/reddb/releases/latest)
    payload='{"tag_name":"v9.9.9"}'
    ;;
  https://api.github.com/repos/reddb-io/reddb/releases/tags/v9.9.9)
    payload='{"tag_name":"v9.9.9"}'
    ;;
  https://github.com/reddb-io/reddb/releases/download/v9.9.9/*)
    asset="\${url##*/}"
    file="$FAKE_RELEASE_DIR/\${asset}"
    if [ ! -f "$file" ]; then
      exit 22
    fi
    if [ "$output" = "-" ] || [ -z "$output" ]; then
      cat "$file"
    else
      cp "$file" "$output"
    fi
    exit 0
    ;;
  *)
    echo "curl fixture: unexpected url $url" >&2
    exit 22
    ;;
esac

if [ "$output" = "-" ] || [ -z "$output" ]; then
  printf '%s' "$payload"
else
  printf '%s' "$payload" > "$output"
fi
`,
  );

  return { dir, releaseDir, binDir, installDir, tmpDir };
}

function runInstaller(fixture, args) {
  return spawnSync("bash", [installer, ...args], {
    cwd: repoRoot,
    env: {
      ...process.env,
      FAKE_RELEASE_DIR: fixture.releaseDir,
      INSTALL_DIR: fixture.installDir,
      PATH: `${fixture.binDir}:${process.env.PATH}`,
      TMPDIR: fixture.tmpDir,
    },
    encoding: "utf8",
  });
}

test("install.sh verifies SHA256SUMS before installing red", (t) => {
  const asset = "red-linux-x86_64-static";
  const bytes = "server-binary";
  const fixture = makeFixture(t, {
    files: { [asset]: bytes },
    checksums: `${sha256Hex(bytes)}  ${asset}\n`,
  });

  const result = runInstaller(fixture, ["--install-dir", fixture.installDir]);

  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /Verified checksum for red-linux-x86_64-static/);
  assert.equal(fs.readFileSync(path.join(fixture.installDir, "red"), "utf8"), bytes);
});

test("install.sh refuses a checksum mismatch", (t) => {
  const asset = "red-linux-x86_64-static";
  const fixture = makeFixture(t, {
    files: { [asset]: "tampered-binary" },
    checksums: `${"0".repeat(64)}  ${asset}\n`,
  });

  const result = runInstaller(fixture, ["--install-dir", fixture.installDir]);

  assert.notEqual(result.status, 0);
  assert.match(result.stdout + result.stderr, /Checksum mismatch for red-linux-x86_64-static/);
  assert.equal(fs.existsSync(path.join(fixture.installDir, "red")), false);
});

test("install.sh refuses a release missing the selected asset in SHA256SUMS", (t) => {
  const asset = "red-linux-x86_64-static";
  const fixture = makeFixture(t, {
    files: { [asset]: "server-binary" },
    checksums: `${sha256Hex("other-binary")}  red-linux-x86_64\n`,
  });

  const result = runInstaller(fixture, ["--install-dir", fixture.installDir]);

  assert.notEqual(result.status, 0);
  assert.match(
    result.stdout + result.stderr,
    /Checksum manifest does not contain red-linux-x86_64-static/,
  );
  assert.equal(fs.existsSync(path.join(fixture.installDir, "red")), false);
});

test("install.sh supports --client-only and --prefix", (t) => {
  const asset = "red_client-linux-x86_64-static";
  const bytes = "client-binary";
  const fixture = makeFixture(t, {
    files: { [asset]: bytes },
    checksums: `${sha256Hex(bytes)}  ${asset}\n`,
  });
  const prefix = path.join(fixture.dir, "prefix");

  const result = runInstaller(fixture, ["--client-only", "--prefix", prefix]);

  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /RedDB thin client installed/);
  assert.equal(fs.readFileSync(path.join(prefix, "bin", "red_client"), "utf8"), bytes);
  assert.equal(fs.existsSync(path.join(prefix, "bin", "red")), false);
});

test("install.sh supports --uninstall without network access", (t) => {
  const fixture = makeFixture(t, {
    files: {},
    checksums: "",
  });
  const prefix = path.join(fixture.dir, "prefix");
  const bin = path.join(prefix, "bin");
  fs.mkdirSync(bin, { recursive: true });
  fs.writeFileSync(path.join(bin, "red_client"), "old-client");

  const result = runInstaller(fixture, ["--client-only", "--prefix", prefix, "--uninstall"]);

  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /Removed:/);
  assert.equal(fs.existsSync(path.join(bin, "red_client")), false);
});
