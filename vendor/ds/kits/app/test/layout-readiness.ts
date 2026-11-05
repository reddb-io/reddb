import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { expect } from "vitest";

const REPO_ROOT = join(import.meta.dirname, "..", "..", "..");

type Readiness = {
  id: string;
  baselineDisposition: string;
  canonicalKit: string;
  export: string;
  evidence: Record<string, string[]>;
};

/** Pin one layout's release evidence at the same seam the producer consumes. */
export function expectLayoutReadiness(id: string, exported: string): void {
  const definitions = JSON.parse(
    readFileSync(join(REPO_ROOT, "scripts", "producer", "readiness.json"), "utf8"),
  ) as Readiness[];
  const definition = definitions.find((candidate) => candidate.id === id);

  expect(definition).toMatchObject({
    id,
    baselineDisposition: "implemented",
    // The Baseline classification a definition declares, which is the direction
    // ("application"); "app" is the subpath it is distributed under, asserted
    // through the barrel below rather than read back out of the declaration.
    canonicalKit: "application",
    export: exported,
  });
  for (const paths of Object.values(definition!.evidence)) {
    expect(paths.length).toBeGreaterThan(0);
    for (const path of paths) expect(existsSync(join(REPO_ROOT, path)), path).toBe(true);
  }

  const snapshot = JSON.parse(
    readFileSync(
      join(REPO_ROOT, "packages", "baseline", "snapshot", "baseline-v1.json"),
      "utf8",
    ),
  ) as { capabilities: Array<{ id: string; classification: string; status: string }> };
  expect(snapshot.capabilities.find((capability) => capability.id === id)).toMatchObject({
    classification: "application",
    status: "implemented",
  });

  const barrel = readFileSync(join(REPO_ROOT, "kits", "app", "src", "index.ts"), "utf8");
  expect(barrel).toContain(`export { default as ${exported} }`);
}
