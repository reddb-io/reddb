import { join } from "node:path";
import { runLint } from "@reddb-io/kit-lint";
import { KIT_ROOT, kitSourceFiles } from "./paths";

const args = process.argv.slice(2);
const files = args.length > 0 ? args : [...kitSourceFiles(), ...kitSourceFiles(join(KIT_ROOT, "react/src"))];

if (files.length === 0) {
  process.stdout.write("anti-hardcode lint: no Data Kit source files to lint.\n");
  process.exit(0);
}

process.exit(runLint(KIT_ROOT, files));
