// Anti-hardcode lint command for the application Kit.
//
//   pnpm --filter kit-app lint            # lint the Kit's own source
//   pnpm --filter kit-app lint <file...>  # lint specific files
//
// Exits non-zero (and prints each offender) when a component carries a value
// no Layer can reassign — a colour or radius the Themes do not own, or a
// spatial step the Density axis ships a role for.
//
// The rule itself is @reddb-io/kit-lint's, shared with every other Kit: what
// is this Kit's own is only which files to read.

import { join } from "node:path";
import { runLint } from "@reddb-io/kit-lint";
import { KIT_ROOT, kitSourceFiles } from "./paths";

const args = process.argv.slice(2);

process.exit(runLint(KIT_ROOT, args.length > 0 ? args : [...kitSourceFiles(), ...kitSourceFiles(join(KIT_ROOT, "react/src"))]));
