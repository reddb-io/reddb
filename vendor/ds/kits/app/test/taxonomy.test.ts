// The mechanical Taxonomy test.
//
// `.red/contexts/component-system/CONTEXT.md` defines a Primitive as "a Kit
// component that imports no other Kit component — the test is mechanical, by
// inspection", and a Composite as one that composes at least one, precisely so
// the split never becomes a debate (Spec #1's ninth user story). This file is
// that inspection, and it is the only authority on where a component belongs:
// read the imports, and the answer follows.
//
// It supersedes the earlier `primitives.test.ts`, which could only ask half of
// the question — when the Kit was Primitives all the way down, "imports no
// other component" was the whole rule. A Composite makes the directory a claim
// rather than a fact, so both halves are now checked in both directions: a
// component under `src/primitives` that reached for another is in the wrong
// place, and so is one under `src/composites` that composes nothing at all.
//
// Discovery is by file, not by list, so a component dropped into either
// directory is judged the moment it exists.

import { readFileSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { BASE_COMPONENTS } from "@reddb-io/design-system/base";
import { COMPONENTS, COMPOSITES, PRIMITIVES } from "../src/index";
import { COMPOSITES_DIR, KIT_ROOT, PRIMITIVES_DIR, kitComponentFiles } from "../tools/paths";

const primitives = kitComponentFiles(PRIMITIVES_DIR);
const composites = kitComponentFiles(COMPOSITES_DIR);
const components = kitComponentFiles();

/** Every module specifier a file imports, static or dynamic. */
function importsOf(source: string): string[] {
  const patterns = [/\bfrom\s+["']([^"']+)["']/g, /\bimport\s*\(\s*["']([^"']+)["']\s*\)/g];
  return patterns.flatMap((pattern) => [...source.matchAll(pattern)].map((match) => match[1]!));
}

/** The Kit components `file` imports — the whole of what decides its half. */
function componentImports(file: string): string[] {
  const source = readFileSync(file, "utf8");
  const relativeComponents = importsOf(source).filter((specifier) => specifier.endsWith(".svelte"));
  const inheritedComponents = [
    ...source.matchAll(
      /\bimport\s*\{([^}]+)\}\s*from\s*["']@reddb-io\/design-system\/base["']/gs,
    ),
  ].flatMap((match) =>
    match[1]!
      .split(",")
      .map((binding) => binding.trim().replace(/^type\s+/, "").split(/\s+as\s+/)[0]!)
      .filter((binding) => BASE_COMPONENTS.includes(binding as never))
      .map((binding) => `@reddb-io/design-system/base#${binding}`),
  );

  return [...relativeComponents, ...inheritedComponents];
}

describe("the Kit's components", () => {
  it("are discovered, not assumed — the globs found files in both halves", () => {
    expect(primitives.length).toBeGreaterThan(0);
    expect(composites.length).toBeGreaterThan(0);
  });

  it("live in one half or the other, and nowhere else", () => {
    // A component outside both directories would be judged by neither rule
    // below, which is the one way this file could pass while saying nothing.
    const placed = new Set([...primitives, ...composites]);
    const stray = components
      .filter((file) => !placed.has(file))
      .map((file) => relative(KIT_ROOT, file));
    expect(stray).toEqual([]);
  });

  it("are exactly the ones the Kit's catalogues declare", () => {
    // The catalogues are what a consumer reads out of vendored source, with no
    // bundler to glob for them; this is what stops them drifting from the files.
    expect(primitives.map((file) => basename(file, ".svelte")).sort()).toEqual(
      [...PRIMITIVES].sort(),
    );
    expect(composites.map((file) => basename(file, ".svelte")).sort()).toEqual(
      [...COMPOSITES].sort(),
    );
    expect([...COMPONENTS].sort()).toEqual([...PRIMITIVES, ...COMPOSITES].sort());
  });
});

describe("every component under src/primitives is a Primitive", () => {
  for (const file of primitives) {
    const name = basename(file);

    it(`${name} imports no other Kit component`, () => {
      expect(componentImports(file)).toEqual([]);
    });

    it(`${name} does not reach back through the Kit's own entry point`, () => {
      // Importing the barrel would pull in every component transitively, which
      // is the same coupling wearing a different specifier.
      const barrel = importsOf(readFileSync(file, "utf8")).filter(
        (specifier) => specifier === "@reddb-io/design-system/app" || /(^|\/)index(\.ts)?$/.test(specifier),
      );
      expect(barrel).toEqual([]);
    });
  }
});

describe("every component under src/composites is a Composite", () => {
  for (const file of composites) {
    const name = basename(file);

    it(`${name} composes at least one Kit component`, () => {
      // The positive half of the rule, and the one that keeps the directory
      // honest: a "Composite" that composes nothing is a Primitive filed in the
      // wrong place, and it would pass a test that only forbade things.
      expect(componentImports(file).length).toBeGreaterThan(0);
    });

    it(`${name} composes explicit local components or inherited Base`, () => {
      // Local composition stays relative; canonical inherited components use
      // Base's explicit public subpath. Neither route reaches back through the
      // Application barrel.
      for (const specifier of componentImports(file)) {
        expect(
          components.includes(resolve(dirname(file),specifier)) ||
            specifier.startsWith("@reddb-io/design-system/base#"),
          `${name} imports ${specifier}`,
        ).toBe(true);
      }
      const barrel = importsOf(readFileSync(file, "utf8")).filter(
        (specifier) => specifier === "@reddb-io/design-system/app" || /(^|\/)index(\.ts)?$/.test(specifier),
      );
      expect(barrel).toEqual([]);
    });
  }
});


describe("local Composite composition", () => {
  it("keeps the local component import graph free of cycles", () => {
    const finished=new Set<string>();
    function visit(file:string, parents:readonly string[]):void {
      expect(parents.includes(file),`component import cycle: ${[...parents,file].map(item=>relative(KIT_ROOT,item)).join(" -> ")}`).toBe(false);
      if(finished.has(file)) return;
      for(const specifier of componentImports(file)) {
        const dependency=resolve(dirname(file),specifier);
        if(components.includes(dependency)) visit(dependency,[...parents,file]);
      }
      finished.add(file);
    }
    for(const file of components) visit(file,[]);
  });
});
