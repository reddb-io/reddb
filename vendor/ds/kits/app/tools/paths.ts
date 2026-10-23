import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { filesUnder, kitSourceFiles as sourceFilesUnder } from "@reddb-io/kit-lint";

// kits/app/tools
const here = dirname(fileURLToPath(import.meta.url));

export const KIT_ROOT = join(here, "..");
/** The vendorable component source — the whole of what a consumer receives. */
export const SRC_DIR = join(KIT_ROOT, "src");
/**
 * Where each half of the Taxonomy lives
 * (`.red/contexts/component-system/CONTEXT.md`). The directory is
 * a claim, not the authority: `test/taxonomy.test.ts` reads the imports and
 * decides which half a component actually belongs to, and fails when the file
 * it is in disagrees.
 */
export const PRIMITIVES_DIR = join(SRC_DIR, "primitives");
export const COMPOSITES_DIR = join(SRC_DIR, "composites");
/** The Kit's routing manifest, read by ds-sync (ADR 0002). */
export const KIT_MANIFEST = join(KIT_ROOT, "kit.json");
/**
 * The tsconfig a consumer's own `svelte-check` stands in for: strict, no DS
 * base config behind it, `src` and nothing else. It is what the Kit is
 * compiled against out there, so it is what the Kit is checked against here.
 */
export const CONSUMER_TSCONFIG = join(KIT_ROOT, "tsconfig.consumer.json");
/** Build output: the vendorable source, staged for the release bundle. */
export const DIST_DIR = join(KIT_ROOT, "dist");

/** Every file under `dir`, recursively, sorted — a stable, reproducible order. */
export { filesUnder };

/**
 * The Kit's component source files: what the lint reads and what the Taxonomy
 * test inspects. Discovered rather than listed, so a component added without a
 * test or with a hardcoded colour is caught the moment its file exists. Which
 * files count is the linter's answer, shared with every other Kit.
 */
export function kitSourceFiles(dir: string = SRC_DIR): string[] {
  return sourceFilesUnder(dir);
}

/** The Kit's Svelte components, by file. */
export function kitComponentFiles(dir: string = SRC_DIR): string[] {
  return filesUnder(dir).filter((file) => file.endsWith(".svelte"));
}
