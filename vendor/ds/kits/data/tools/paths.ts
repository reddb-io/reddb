import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { filesUnder, kitSourceFiles as sourceFilesUnder } from "@reddb-io/kit-lint";

const here = dirname(fileURLToPath(import.meta.url));

export const KIT_ROOT = join(here, "..");
export const SRC_DIR = join(KIT_ROOT, "src");
export const KIT_MANIFEST = join(KIT_ROOT, "kit.json");
export const DIST_DIR = join(KIT_ROOT, "dist");

export { filesUnder };

export function kitSourceFiles(dir: string = SRC_DIR): string[] {
  return sourceFilesUnder(dir);
}
