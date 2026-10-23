// What a marker has to be, written as functions instead of assertions.
//
// `tree-shaking.test.ts` attributes a bundle by looking in it for class
// strings the Kits' components write. That reading — "the marker is here, so
// the component is here" — only holds while two properties do:
//
//   coverage        every catalogued component has a marker, or the bundle
//                   assertions quietly stop measuring the ones that do not;
//   distinctiveness no marker can be written by a second component, or a hit
//                   attributes the bundle to the wrong one.
//
// Both used to be asserted inline in that file and neither was itself tested,
// which is how coverage drifted the moment Diff landed (#158) and stayed
// drifted until the two lists were compared by eye. They live here so
// `markers.test.ts` can hold them against a fixture catalogue built to break
// them: a guard nothing exercises is a guard nobody knows the shape of.

import { readFileSync } from "node:fs";
import { basename } from "node:path";

/** A marker map's key (`ProductQuickview`) as the catalogue names it. */
export function kebab(name: string): string {
  return name.replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase();
}

/**
 * The component a source file belongs to.
 *
 * A component is spread across files that share a stem — `Kbd.svelte`,
 * `kbd.variants.ts`, `dropdown-menu.behavior.ts` — and every one of them can
 * put a class string into a bundle, so every one of them counts as that
 * component's output. Reading only the `.variants.ts` half would call a marker
 * distinctive while a neighbour's markup writes it inline.
 */
export function componentOf(file: string): string {
  const stem = basename(file)
    .replace(/\.(svelte|ts)$/, "")
    .replace(/\.(variants|behavior|appearance)$/, "");
  return kebab(stem);
}

/** A source file, paired with the component it would put into a bundle. */
export interface Emitter {
  readonly component: string;
  readonly text: string;
}

/** Read `files` once, so a whole marker map can be checked against them. */
export function emitters(files: readonly string[]): Emitter[] {
  return files.map((file) => ({
    component: componentOf(file),
    text: readFileSync(file, "utf8"),
  }));
}

/**
 * Every component whose own source writes `marker`, sorted.
 *
 * One is the only answer that lets a marker attribute a bundle. Two means the
 * string is an idiom rather than a signature, and `bundle.includes(marker)`
 * has started answering a question nobody asked; zero means the marker has
 * been edited out from under the map and every assertion using it is vacuous.
 */
export function componentsWriting(marker: string, sources: readonly Emitter[]): string[] {
  const writers = sources.filter((source) => source.text.includes(marker));
  return [...new Set(writers.map((source) => source.component))].sort();
}

/** Where a catalogue and a marker map disagree about what is measured. */
export interface Coverage {
  /** Catalogued components no marker measures — a silent hole in the suite. */
  readonly unmarked: string[];
  /** Markers naming no catalogued component — a measurement of nothing. */
  readonly uncatalogued: string[];
}

/**
 * Compare the components that exist against the components that are measured.
 *
 * Reported as two lists rather than one equality, because the two failures
 * want different repairs: something added to the Kit needs a marker, and a
 * marker left behind by a removed component needs deleting.
 */
export function coverage(
  catalogue: readonly string[],
  markers: Readonly<Record<string, string>>,
): Coverage {
  const marked = new Set(Object.keys(markers).map(kebab));
  const catalogued = new Set(catalogue);
  return {
    unmarked: [...catalogued].filter((name) => !marked.has(name)).sort(),
    uncatalogued: [...marked].filter((name) => !catalogued.has(name)).sort(),
  };
}
