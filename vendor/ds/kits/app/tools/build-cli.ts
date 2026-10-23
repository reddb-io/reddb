// Build command for the application Kit.
//
//   pnpm --filter kit-app build
//
// A Kit's artifact is its source: components are distributed as vendorable
// Svelte 5 source (ADR 0002), not as a compiled bundle, so "building" is
// staging exactly what a consumer receives into dist/ — the directory the
// release bundler collects from every package and Kit
// (scripts/producer/src/collect.ts).
//
// Unlike the Layers' artifacts, this dist is a verbatim copy of committed
// source and carries no information of its own, so it is generated on demand
// and git-ignored rather than committed. Copying in sorted order with the
// file list printed keeps the same commit producing the same bundle, which is
// what the release reproducibility check depends on.

import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { basename, join, relative } from "node:path";
import { DIST_DIR, KIT_MANIFEST, KIT_ROOT, SRC_DIR, filesUnder } from "./paths";

rmSync(DIST_DIR, { recursive: true, force: true });
mkdirSync(DIST_DIR, { recursive: true });

cpSync(SRC_DIR, join(DIST_DIR, "src"), { recursive: true });
if (existsSync(join(KIT_ROOT, "react"))) cpSync(join(KIT_ROOT, "react"), join(DIST_DIR, "react"), { recursive: true });
for (const file of [KIT_MANIFEST, join(KIT_ROOT, "README.md")]) {
  cpSync(file, join(DIST_DIR, basename(file)));
}

const staged = filesUnder(DIST_DIR);
for (const file of staged) {
  process.stdout.write(`Staged ${relative(KIT_ROOT, file)}\n`);
}
process.stdout.write(`Wrote ${staged.length} file(s) to ${DIST_DIR}\n`);
