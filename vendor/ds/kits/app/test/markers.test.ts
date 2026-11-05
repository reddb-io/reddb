// The guard that keeps `tree-shaking.test.ts` honest, held against a catalogue
// built to break it.
//
// That file measures a real build, so its own failures take two production
// bundles to reach and read as bundle problems whatever their cause. The two
// properties a marker map has to keep — every component measured, every marker
// naming one component — are decided before any of that, from source text
// alone, so they are decided here instead: against `test/fixtures/markers`,
// two stand-in components that share an idiom and where only one is marked.
//
// What the fixture demonstrates is the red state. `fixture-chip` is in the
// catalogue and absent from the marker map, exactly as `diff` was after #158,
// and the first test below is the assertion that would have caught it the day
// it landed.

import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { componentOf, componentsWriting, coverage, emitters, kebab } from "./markers";

const FIXTURES = join(import.meta.dirname, "fixtures", "markers");

/** A catalogue of two components, the way the real one is read off disk. */
const CATALOGUE = ["fixture-badge", "fixture-chip"];

/** An idiom both stand-ins write, and so a string that attributes nothing. */
const SHARED = "inline-flex rounded-full border";

const sources = emitters([
  join(FIXTURES, "fixture-badge.variants.ts"),
  join(FIXTURES, "fixture-chip.variants.ts"),
]);

describe("a catalogue measured by a marker map", () => {
  it("reports a component that was added without a marker", () => {
    // The #158 shape: a component ships, the map is not extended, and every
    // bundle assertion silently stops covering it.
    expect(coverage(CATALOGUE, { FixtureBadge: "fixture-badge-cap" })).toEqual({
      unmarked: ["fixture-chip"],
      uncatalogued: [],
    });
  });

  it("reports a marker left behind by a component that is gone", () => {
    expect(
      coverage(["fixture-badge"], { FixtureBadge: "fixture-badge-cap", FixtureChip: SHARED }),
    ).toEqual({ unmarked: [], uncatalogued: ["fixture-chip"] });
  });

  it("is satisfied when the two lists name the same components", () => {
    expect(
      coverage(CATALOGUE, { FixtureBadge: "fixture-badge-cap", FixtureChip: "fixture-chip-cap" }),
    ).toEqual({ unmarked: [], uncatalogued: [] });
  });

  it("compares the map's names in the catalogue's spelling", () => {
    // The map is keyed the way a component is imported, the catalogue the way
    // its files are named; a comparison that skipped the conversion would call
    // every multi-word component uncovered.
    expect(kebab("ProductQuickview")).toBe("product-quickview");
    expect(kebab("Kbd")).toBe("kbd");
  });
});

describe("a marker held against the source that can write it", () => {
  it("names one component when only that component writes it", () => {
    expect(componentsWriting("fixture-badge-cap", sources)).toEqual(["fixture-badge"]);
  });

  it("names both components when either can write it", () => {
    // What a non-distinctive marker looks like from here, and the reason the
    // real suite asserts a single name rather than a non-empty list: a bundle
    // containing this string is evidence for neither component.
    expect(componentsWriting(SHARED, sources)).toEqual(["fixture-badge", "fixture-chip"]);
  });

  it("names nothing when the marker has been edited out from under the map", () => {
    expect(componentsWriting("fixture-badge-brim", sources)).toEqual([]);
  });
});

describe("the component a source file belongs to", () => {
  it("gathers every file that shares a stem", () => {
    // A component puts class strings into a bundle from all of its files, so
    // a marker written in one of them is that component's either way.
    expect(["Kbd.svelte", "kbd.variants.ts"].map(componentOf)).toEqual(["kbd", "kbd"]);
    expect(componentOf("dropdown-menu.behavior.ts")).toBe("dropdown-menu");
  });

  it("reads a PascalCase component file as its catalogue name", () => {
    expect(componentOf(join("src", "composites", "ProductQuickview.svelte"))).toBe(
      "product-quickview",
    );
  });
});
