// The coherence rules wave 3a moved the Base Kit onto, read off its source.
//
//   - Focus is ink (ADR 0023): every focus indicator a Base class draws is the
//     `focus` role as an outline, never the Brand red, a feedback ring, or
//     any other ring colour.
//   - Container-first (ADR 0021): no Base component steps on the viewport.
//     Base ships no page chrome that would be allowed to.
//   - Layout primitives own their rhythm (ADR 0024): Form, Fieldset and Stack
//     draw the space between their own children from the layout tier.
//   - One event dialect (ADR 0026): every DS callback is lowercase and is
//     `onvaluechange`, `onopenchange`, `on<state>change` for a bindable state,
//     or a discrete `on<noun>`. A native name (`onchange`, `oninput`,
//     `onclick`) carries its native Event, or is a deprecated alias.
//     `onselect` — item activation — is the sanctioned exception.

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  descriptionList,
  fieldset,
  form,
  gridList,
  navbar,
  sectionHeading,
  stack,
} from "@reddb-io/design-system/base";
import { classesOf } from "./mount";

const SOURCE = join(import.meta.dirname, "..", "src");
const files = readdirSync(SOURCE)
  .filter((name) => /\.(?:ts|svelte)$/.test(name))
  .map((name) => ({ name, text: readFileSync(join(SOURCE, name), "utf8") }));

/** Every class-like token in a file's string literals and class attributes. */
function tokens(text: string): string[] {
  return [...text.matchAll(/["'`]([^"'`\n]*)["'`]/g)].flatMap(([, literal]) => literal!.split(/\s+/));
}

describe("the Base Kit's focus indicators", () => {
  it("draw only the ink focus role, as a 2px outline at a 2px offset", () => {
    const offenders = files.flatMap(({ name, text }) =>
      tokens(text)
        .filter((token) => /focus(?:-visible|-within)?:/.test(token))
        .filter((token) => /:(?:ring-(?!0\b)|outline-(?!none\b|hidden\b|2\b|offset-2\b|focus\b))/.test(token))
        .map((token) => `${name}: ${token}`),
    );
    expect(offenders).toEqual([]);

    const focusing = files.filter(({ text }) => tokens(text).some((token) => /focus-visible:outline-focus$/.test(token)));
    // Every file that styles focus-visible styles it with the role.
    for (const { name, text } of files) {
      const styles = tokens(text).some((token) => /(?:^|:)focus-visible:(?:outline|ring)-/.test(token));
      const outlines = tokens(text).filter((token) => /focus-visible:outline-(?:2|offset-2|focus)$/.test(token));
      if (!styles || name === "dialog.variants.ts") continue;
      expect(outlines.length, `${name} draws the focus outline`).toBeGreaterThanOrEqual(3);
    }
    expect(focusing.length).toBeGreaterThanOrEqual(17);
  });
});

describe("the Base Kit's responsive steps", () => {
  it("never query the viewport: every step is a container step", () => {
    const viewport = /^(?:[a-z-]+:)*(?:max-)?(?:sm|md|lg|xl|2xl):/;
    const offenders = files.flatMap(({ name, text }) =>
      tokens(text).filter((token) => viewport.test(token)).map((token) => `${name}: ${token}`),
    );
    expect(offenders).toEqual([]);
  });

  it("declare a named size container where the layout steps", () => {
    expect(classesOf(descriptionList().root()).has("@container/description-list")).toBe(true);
    expect(classesOf(navbar().root()).has("@container/navbar")).toBe(true);
    expect(classesOf(sectionHeading().root()).has("@container/section-heading")).toBe(true);
    // GridList steps intrinsically on its own width — no viewport, no wrapper.
    for (const columns of [2, 3, 4] as const) {
      const worn = gridList({ columns });
      expect(worn).toMatch(/grid-cols-\[repeat\(auto-fill,/);
      expect(worn).toContain("var(--reddb-container-lg)");
    }
  });
});

describe("the Base layout primitives", () => {
  it("own the rhythm between their children from the layout tier", () => {
    expect(classesOf(form()).has("gap-[var(--reddb-spatial-layout-gap-md)]")).toBe(true);
    expect(classesOf(fieldset().root()).has("gap-[var(--reddb-spatial-layout-gap-sm)]")).toBe(true);
    expect(classesOf(fieldset().legend()).has("mb-[var(--reddb-spatial-layout-gap-sm)]")).toBe(true);
    expect(classesOf(stack({ gap: "section" })).has("gap-[var(--reddb-spatial-section-gap)]")).toBe(true);
  });
});

describe("the Base Kit's event dialect", () => {
  /** Discrete DS events: a noun, never a native event name — except `onselect`. */
  const DISCRETE = new Set(["ondismiss", "oncomplete", "onconfirm"]);
  /** The sanctioned exception (maintainer decision, 2026-10-06): item activation. */
  const SANCTIONED = new Set(["onselect"]);
  const NATIVE = new Set(["onchange", "oninput", "onclick"]);

  it("names every callback in the one dialect, with onselect as the sanctioned exception", () => {
    const offenders: string[] = [];
    let sanctioned = 0;
    for (const { name, text } of files.filter((file) => file.name.endsWith(".svelte") || file.name.endsWith(".ts"))) {
      const lines = text.split("\n");
      lines.forEach((line, index) => {
        const match = /^\s+(on[A-Za-z]+)\?:\s*(.*)$/.exec(line);
        if (!match) return;
        const [, callback, type] = match as unknown as [string, string, string];
        const deprecated = /@deprecated/.test(lines[index - 1] ?? "");
        const where = `${name}: ${callback}`;
        if (callback !== callback.toLowerCase()) offenders.push(`${where} is not lowercase`);
        else if (SANCTIONED.has(callback)) sanctioned += 1;
        else if (NATIVE.has(callback)) {
          // A native name keeps its native Event, or it is a one-release alias.
          if (!/Attributes\["on/.test(type) && !deprecated) offenders.push(`${where} reuses a native name: ${type}`);
        } else if (!/^on[a-z]*change$/.test(callback) && !DISCRETE.has(callback)) {
          offenders.push(`${where} is outside the dialect`);
        }
      });
    }
    expect(offenders).toEqual([]);
    expect(sanctioned).toBeGreaterThan(0);
  });
});
