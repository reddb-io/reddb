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

/** Pin one commerce surface at the same release seams the producer consumes. */
export function expectCommerceReadiness(id: string, exported: string): void {
  const definitions = JSON.parse(
    readFileSync(join(REPO_ROOT, "scripts", "producer", "readiness.json"), "utf8"),
  ) as Readiness[];
  const definition = definitions.find((candidate) => candidate.id === id);

  expect(definition).toMatchObject({
    id,
    baselineDisposition: "implemented",
    canonicalKit: "application",
    export: exported,
  });
  expect(Object.keys(definition!.evidence).sort()).toEqual([
    "accessibility",
    "appearance",
    "behavior",
    "consumerCompilation",
    "documentation",
    "showcase",
  ]);
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
