// Consumer type-check command for the Kit.
//
//   pnpm --filter kit-app check
//
// Compiles the vendorable source the way a consumer application does — the
// consumer's svelte-check, a strict tsconfig with no DS base behind it — and
// exits non-zero on anything it finds. Mirrors the anti-hardcode lint command
// (tools/lint-cli.ts) down to its output shape, because it is the same kind of
// promise: what this repo ships still works where it lands.

import { relative } from "node:path";
import { consumerCheck, formatDiagnostic } from "./consumer-check";
import { CONSUMER_TSCONFIG, KIT_ROOT } from "./paths";

const tsconfig = process.argv[2] ?? CONSUMER_TSCONFIG;
const { errors, warnings } = consumerCheck(tsconfig);
const problems = [...errors, ...warnings];

if (problems.length > 0) {
  for (const problem of problems) {
    process.stderr.write(`${formatDiagnostic(problem)}\n`);
  }
  process.stderr.write(
    `\nconsumer type check failed: ${errors.length} error(s) and ${warnings.length} warning(s) ` +
      "against a strict consumer tsconfig; a Kit ships as source, so this is the compiler that judges it.\n",
  );
  process.exit(1);
}

process.stdout.write(
  "consumer type check passed: the Kit's source compiles clean under a strict consumer " +
    `svelte-check (${relative(KIT_ROOT, tsconfig)}).\n`,
);
