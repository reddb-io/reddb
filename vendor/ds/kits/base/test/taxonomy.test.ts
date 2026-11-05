// Primitive vs Composite is mechanical (Component System glossary): a Primitive
// imports no other Kit component. The Base Kit's catalogues are read against
// the imports themselves, so a component cannot be listed in the wrong half —
// the hand-kept list is the claim, the imports are the evidence.

import { readFileSync, readdirSync } from "node:fs";
import { basename, join } from "node:path";
import { describe, expect, it } from "vitest";
import { BASE_COMPONENTS, BASE_COMPOSITES, BASE_PRIMITIVES } from "../src/index";

const SRC = join(import.meta.dirname, "..", "src");
const components = new Set<string>(BASE_COMPONENTS);

/** The sibling Kit components a Base component file imports. */
function siblingComponents(name: string): string[] {
  const source = readFileSync(join(SRC, `${name}.svelte`), "utf8");
  return [...source.matchAll(/\bfrom\s+["']\.\/([A-Za-z0-9]+)\.svelte["']/g)]
    .map((match) => match[1]!)
    .filter((imported) => components.has(imported) && imported !== name);
}

describe("the Base Kit taxonomy, read from the imports", () => {
  it("catalogues every component file in src", () => {
    const files = readdirSync(SRC)
      .filter((file) => file.endsWith(".svelte"))
      .map((file) => basename(file, ".svelte"))
      .sort();
    expect([...BASE_COMPONENTS].sort()).toEqual(files);
  });

  for (const name of BASE_COMPONENTS) {
    const composes = siblingComponents(name);
    const listed = (BASE_COMPOSITES as readonly string[]).includes(name) ? "Composite" : "Primitive";
    const actual = composes.length > 0 ? "Composite" : "Primitive";

    it(`${name} is listed as a ${actual}${composes.length ? ` (composes ${composes.join(", ")})` : ""}`, () => {
      expect(listed).toBe(actual);
      expect((BASE_PRIMITIVES as readonly string[]).includes(name)).toBe(actual === "Primitive");
    });
  }
});
