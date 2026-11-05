import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { filesUnder, kitSourceFiles as sourceFilesUnder } from "@reddb-io/kit-lint";

// kits/base/tools
const here = dirname(fileURLToPath(import.meta.url));

export const KIT_ROOT = join(here, "..");
/** The vendorable component source — the whole of what a consumer receives. */
export const SRC_DIR = join(KIT_ROOT, "src");
/**
 * The Kit's own copy of the Brand's Marks, vendored inside `src` because that
 * is the whole of what a consumer receives: a Logo whose drawing lived outside
 * the Kit would arrive out there with nothing to render. The bytes are the
 * pinned release's, checked against `vendor/brand/brand.lock.json` by
 * `test/marks.test.ts` — one pin, two places it has to hold.
 */
export const MARKS_DIR = join(SRC_DIR, "marks");
/** The Kit's routing manifest, read by ds-sync (ADR 0002). */
export const KIT_MANIFEST = join(KIT_ROOT, "kit.json");
/** Build output: the vendorable source, staged for the release bundle. */
export const DIST_DIR = join(KIT_ROOT, "dist");
/** The repo's vendored Brand release — the Marks' single upstream. */
export const VENDOR_DIR = join(KIT_ROOT, "..", "..", "vendor", "brand");

/** Every file under `dir`, recursively, sorted — a stable, reproducible order. */
export { filesUnder };

/**
 * The Kit's lintable source. Which files count is the linter's answer, shared
 * with every other Kit (@reddb-io/kit-lint) — the Marks are not among them,
 * because a Mark's colours are the Brand's drawing and not a Kit hardcoding one.
 */
export function kitSourceFiles(dir: string = SRC_DIR): string[] {
  return sourceFilesUnder(dir);
}

/** The Kit's Svelte components, by file. */
export function kitComponentFiles(dir: string = SRC_DIR): string[] {
  return filesUnder(dir).filter((file) => file.endsWith(".svelte"));
}

/** The Marks this Kit ships, by file. */
export function kitMarkFiles(dir: string = MARKS_DIR): string[] {
  return filesUnder(dir).filter((file) => file.endsWith(".svg"));
}
