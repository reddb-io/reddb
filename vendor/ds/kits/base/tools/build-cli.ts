// Build command for the Base Kit.
//
//   pnpm --filter kit-base build
//
// A Kit's artifact is its source (ADR 0002), so "building" is staging exactly
// what a consumer receives into dist/ — the directory the release bundler
// collects from every package and Kit (scripts/producer/src/collect.ts). Same
// shape as the application Kit's build, for the same reason: the release
// bundle must be reproducible from a commit, so the copy is made in sorted
// order and the file list is printed.

import { cpSync, mkdirSync, rmSync } from "node:fs";
import { basename, join, relative } from "node:path";
import { DIST_DIR, KIT_MANIFEST, KIT_ROOT, SRC_DIR, filesUnder } from "./paths";

rmSync(DIST_DIR, { recursive: true, force: true });
mkdirSync(DIST_DIR, { recursive: true });

cpSync(SRC_DIR, join(DIST_DIR, "src"), { recursive: true });
cpSync(join(KIT_ROOT, "react"), join(DIST_DIR, "react"), { recursive: true });
for (const file of [KIT_MANIFEST, join(KIT_ROOT, "README.md")]) {
  cpSync(file, join(DIST_DIR, basename(file)));
}

const staged = filesUnder(DIST_DIR);
for (const file of staged) {
  process.stdout.write(`Staged ${relative(KIT_ROOT, file)}\n`);
}
process.stdout.write(`Wrote ${staged.length} file(s) to ${DIST_DIR}\n`);
