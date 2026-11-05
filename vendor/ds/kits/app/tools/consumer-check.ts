// The consumer type check for the Kit (issue #50).
//
// A Kit ships as source, so the compiler that judges it is the consumer's, not
// the DS's. The DS's own gate never type-checks a .svelte file — vitest mounts
// components through the Svelte compiler, which strips types rather than
// checking them — so a component could compile, mount, click and pass every
// test here while failing the first `svelte-check` it met in an application.
// That is exactly what happened: three type errors reached a consumer's
// Adoption PR that nothing in this repo could have caught.
//
// This runs the consumer's tool with the consumer's settings against the
// vendorable source, so "compiles for a consumer" becomes a checked fact.

import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { CONSUMER_TSCONFIG, KIT_ROOT } from "./paths";

/** One diagnostic, as `--output machine` reports it. */
export interface Diagnostic {
  severity: "ERROR" | "WARNING";
  /** Path relative to the Kit root, exactly as svelte-check prints it. */
  file: string;
  line: number;
  column: number;
  message: string;
}

export interface ConsumerCheckResult {
  diagnostics: Diagnostic[];
  errors: Diagnostic[];
  warnings: Diagnostic[];
  /** svelte-check's raw output, for a CLI that wants to print the whole story. */
  raw: string;
}

const SVELTE_CHECK = join(KIT_ROOT, "node_modules", ".bin", "svelte-check");

// `<timestamp> ERROR "<file>" <line>:<column> "<json-escaped message>"`
const MACHINE_LINE = /^\d+ (ERROR|WARNING) "([^"]*)" (\d+):(\d+) (".*")$/;

/**
 * Parse `--output machine`. Unrecognised lines (START, COMPLETED, and anything
 * a future svelte-check adds) are skipped rather than guessed at: a diagnostic
 * this cannot read is one it must not silently swallow, which is what the
 * COMPLETED-count reconciliation in `consumerCheck` is for.
 */
export function parseMachineOutput(output: string): Diagnostic[] {
  const diagnostics: Diagnostic[] = [];
  for (const line of output.split("\n")) {
    const match = MACHINE_LINE.exec(line.trim());
    if (!match) continue;
    diagnostics.push({
      severity: match[1] as Diagnostic["severity"],
      file: match[2]!,
      line: Number(match[3]),
      column: Number(match[4]),
      message: JSON.parse(match[5]!) as string,
    });
  }
  return diagnostics;
}

/** The `<n> ERRORS <n> WARNINGS` svelte-check itself counted, if it said so. */
function reportedCounts(output: string): { errors: number; warnings: number } | undefined {
  const match = /COMPLETED \d+ FILES (\d+) ERRORS (\d+) WARNINGS/.exec(output);
  if (!match) return undefined;
  return { errors: Number(match[1]), warnings: Number(match[2]) };
}

/**
 * Run svelte-check over the Kit against a consumer-style tsconfig.
 *
 * svelte-check exits non-zero when it finds anything, which is the point, so a
 * failing exit is a result and not an error — only a run that produced no
 * readable report at all is thrown.
 */
export function consumerCheck(tsconfig: string = CONSUMER_TSCONFIG): ConsumerCheckResult {
  let raw: string;
  try {
    raw = execFileSync(SVELTE_CHECK, ["--tsconfig", tsconfig, "--output", "machine"], {
      cwd: KIT_ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (err) {
    const failure = err as { stdout?: string; stderr?: string; message: string };
    raw = failure.stdout ?? "";
    if (reportedCounts(raw) === undefined) {
      throw new Error(
        `svelte-check did not complete against ${tsconfig}: ${failure.stderr || failure.message}`,
      );
    }
  }

  const diagnostics = parseMachineOutput(raw);
  const errors = diagnostics.filter((diagnostic) => diagnostic.severity === "ERROR");
  const warnings = diagnostics.filter((diagnostic) => diagnostic.severity === "WARNING");

  // svelte-check counts what it found; this counts what it could read back. A
  // gap means a diagnostic went unparsed, and a check that loses diagnostics
  // passes for the wrong reason — the one failure mode worth being loud about.
  const reported = reportedCounts(raw);
  if (reported && (reported.errors !== errors.length || reported.warnings !== warnings.length)) {
    throw new Error(
      `svelte-check reported ${reported.errors} error(s) and ${reported.warnings} warning(s) but ` +
        `${errors.length} and ${warnings.length} could be parsed. Its machine output has changed shape:\n${raw}`,
    );
  }

  return { diagnostics, errors, warnings, raw };
}

/** A diagnostic as a human reads it: `file:line:column  message`. */
export function formatDiagnostic(diagnostic: Diagnostic): string {
  const message = diagnostic.message.split("\n").join("\n    ");
  return `${diagnostic.file}:${diagnostic.line}:${diagnostic.column}  ${diagnostic.severity}  ${message}`;
}
