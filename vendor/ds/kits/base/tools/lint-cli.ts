// Anti-hardcode lint command for the Base Kit.
//
//   pnpm --filter kit-base lint            # lint the Kit's own source
//   pnpm --filter kit-base lint <file...>  # lint specific files
//
// The Kit's README said this rule would arrive by EXTRACTING the application
// Kit's linter into something both Kits run, rather than by copying it. This
// file is what is left after that: the rule, the vocabulary and the reporting
// are @reddb-io/kit-lint's, and what is the Base Kit's own is only which files
// to read.

import { runLint } from "@reddb-io/kit-lint";
import { join } from "node:path";
import { KIT_ROOT, kitSourceFiles } from "./paths";

const args = process.argv.slice(2);

process.exit(runLint(KIT_ROOT, args.length > 0 ? args : [...kitSourceFiles(), ...kitSourceFiles(join(KIT_ROOT, "react/src"))]));
