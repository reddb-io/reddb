// The coherence rules wave 3b moved the application Kit onto, read off its
// source — the same rules, and the same reading, as the Base Kit's own
// `kits/base/test/coherence.test.ts`.
//
//   - Focus is ink (ADR 0023): every focus indicator an App class draws is the
//     `focus` role as an outline, never the Brand red or any ring colour.
//   - Container-first (ADR 0021): no App component steps on the viewport,
//     except the page chrome ADR 0021 reserves it for — SidebarLayout's rail
//     frame, whose compact presentation is a drawer fixed to the viewport.
//   - Layout primitives own their rhythm (ADR 0024): the App's layout and
//     commerce composites space their own blocks from the layout tier, and
//     never re-space a Base Form or Fieldset.
//   - One event dialect (ADR 0026): every DS callback is lowercase and is
//     `onvaluechange`, `onopenchange`, `on<state>change`, or a discrete
//     `on<noun>`; `onselect` — item activation — is the sanctioned exception.

import { readFileSync } from "node:fs";
import { relative } from "node:path";
import { describe, expect, it } from "vitest";
import {
  checkoutForm,
  multiColumnLayout,
  orderHistory,
  orderSummary,
  productFeature,
  productOverview,
  productQuickview,
  shoppingCart,
  sidebarLayout,
  splitView,
} from "../src/index";
import { kitSourceFiles, SRC_DIR } from "../tools/paths";
import { classesOf } from "./mount";

const files = kitSourceFiles()
  .filter((file) => /\.(?:ts|svelte)$/.test(file))
  .map((file) => ({ name: relative(SRC_DIR, file), text: readFileSync(file, "utf8") }));

/** Every class-like token in a file's string literals and class attributes. */
function tokens(text: string): string[] {
  return [...text.matchAll(/["'`]([^"'`\n]*)["'`]/g)].flatMap(([, literal]) => literal!.split(/\s+/));
}

const VIEWPORT = /^(?:[a-z-]+:)*(?:max-)?(?:sm|md|lg|xl|2xl):/;

describe("the source this file reads", () => {
  it("is the Kit's components and their variants, both kinds", () => {
    expect(files.filter(({ name }) => name.endsWith(".svelte")).length).toBeGreaterThan(30);
    expect(files.filter(({ name }) => name.endsWith(".variants.ts")).length).toBeGreaterThan(30);
  });
});

describe("the application Kit's focus indicators", () => {
  it("draw only the ink focus role, as a 2px outline at a 2px offset", () => {
    const offenders = files.flatMap(({ name, text }) =>
      tokens(text)
        .filter((token) => /focus(?:-visible|-within)?:/.test(token))
        // `-outline-offset-2` is the same outline drawn inside a control a
        // scroll region would otherwise clip (SidebarRail's tiles).
        .filter((token) => /:(?:ring-(?!0\b)|-?outline-(?!none\b|hidden\b|2\b|offset-2\b|focus\b))/.test(token))
        .map((token) => `${name}: ${token}`),
    );
    expect(offenders).toEqual([]);

    // Every file that styles focus-visible styles it with the role.
    for (const { name, text } of files) {
      const styles = tokens(text).some((token) => /(?:^|:)focus-visible:(?:outline|ring)-/.test(token));
      if (!styles) continue;
      const outlines = tokens(text).filter((token) => /focus-visible:-?outline-(?:2|offset-2|focus)$/.test(token));
      expect(outlines.length, `${name} draws the focus outline`).toBeGreaterThanOrEqual(3);
    }
  });

  it("never names the Brand red or a feedback colour as a focus ring", () => {
    const red = files.flatMap(({ name, text }) =>
      tokens(text)
        .filter((token) => /(?:ring|outline)-(?:primary|feedback-)/.test(token))
        .map((token) => `${name}: ${token}`),
    );
    expect(red).toEqual([]);
  });
});

describe("the application Kit's responsive steps", () => {
  it("never query the viewport outside SidebarLayout's rail chrome", () => {
    const offenders = files.flatMap(({ name, text }) =>
      tokens(text)
        .filter((token) => VIEWPORT.test(token))
        .filter(() => !/(?:^|\/)(?:sidebar-layout\.variants\.ts|SidebarLayout\.svelte)$/.test(name))
        .map((token) => `${name}: ${token}`),
    );
    expect(offenders).toEqual([]);
  });

  it("keep the panel SidebarLayout container-first, and the viewport for the rail frame alone", () => {
    for (const side of ["start", "end"] as const) {
      const panel = sidebarLayout({ side, rail: false });
      const worn = [panel.root(), panel.region(), panel.sidebar(), panel.content()].flatMap((name) => [
        ...classesOf(name),
      ]);
      expect(worn.filter((name) => VIEWPORT.test(name)), side).toEqual([]);
      expect(classesOf(panel.root()).has("@container/sidebar-layout")).toBe(true);
      expect(classesOf(panel.sidebar()).has("@3xl/sidebar-layout:basis-0")).toBe(true);
      expect(classesOf(panel.content()).has("@3xl/sidebar-layout:basis-0")).toBe(true);
    }
  });

  it("declare a named size container where the layout steps", () => {
    expect(classesOf(multiColumnLayout().root()).has("@container/multi-column-layout")).toBe(true);
    expect(classesOf(multiColumnLayout({ columns: "three" }).end()).has("@5xl/multi-column-layout:basis-0")).toBe(
      true,
    );
    expect(classesOf(productFeature().root()).has("@container/product-feature")).toBe(true);
    expect(classesOf(splitView().pane()).has("@container/split-pane")).toBe(true);
  });

  it("step the two-column commerce grids intrinsically on their own width", () => {
    for (const worn of [productOverview().root(), productQuickview().content()]) {
      const track = [...classesOf(worn)].find((name) => name.startsWith("grid-cols-"));
      expect(track).toMatch(/^grid-cols-\[repeat\(auto-fill,/);
      expect(track).toContain("var(--reddb-container-3xl)");
      expect(track).toContain("var(--reddb-spatial-layout-gap-md)");
    }
    // A cart item wraps its controls below its details on its own width.
    expect(classesOf(shoppingCart().item()).has("flex-wrap")).toBe(true);
  });
});

describe("the application layout composites", () => {
  it("space their own blocks from the layout tier", () => {
    const LAYOUT_MD = "gap-[var(--reddb-spatial-layout-gap-md)]";
    const LAYOUT_SM = "gap-[var(--reddb-spatial-layout-gap-sm)]";
    expect(classesOf(multiColumnLayout().root()).has(LAYOUT_MD)).toBe(true);
    expect(classesOf(sidebarLayout({ rail: false }).root()).has(LAYOUT_MD)).toBe(true);
    expect(classesOf(productOverview().root()).has(LAYOUT_MD)).toBe(true);
    expect(classesOf(productQuickview().content()).has(LAYOUT_MD)).toBe(true);
    expect(classesOf(productFeature().root()).has(LAYOUT_MD)).toBe(true);
    expect(classesOf(orderHistory().root()).has(LAYOUT_SM)).toBe(true);
    expect(classesOf(orderSummary().content()).has(LAYOUT_SM)).toBe(true);
  });

  it("never re-space the Base Form and Fieldset they compose", () => {
    for (const worn of [checkoutForm().root(), checkoutForm().section(), shoppingCart().root()]) {
      expect([...classesOf(worn)].filter((name) => /^(?:gap|flex|grid)(?:-|$)/.test(name))).toEqual([]);
    }
  });
});

describe("the application Kit's event dialect", () => {
  /** Discrete DS events: a noun, never a native event name — except `onselect`. */
  // PlaybackControls emits a discrete step request; the application owns execution.
  const DISCRETE = new Set(["ondismiss", "onremove", "onstep", "oncloserequest", "onapplyrequest", "ondiscardrequest", "onfailure", "onretry"]);
  /** The sanctioned exception (maintainer decision, 2026-10-06): item activation. */
  const SANCTIONED = new Set(["onselect"]);
  const NATIVE = new Set(["onchange", "oninput", "onclick", "onclose", "onerror"]);

  it("names every callback in the one dialect, with onselect as the sanctioned exception", () => {
    const offenders: string[] = [];
    let sanctioned = 0;
    for (const { name, text } of files) {
      const lines = text.split("\n");
      lines.forEach((line, index) => {
        const match = /^\s+(on[A-Za-z]+)\?:\s*(.*)$/.exec(line);
        if (!match) return;
        const [, callback, type] = match as unknown as [string, string, string];
        const deprecated = /@deprecated/.test(lines[index - 1] ?? "");
        const where = `${name}: ${callback}`;
        if (deprecated) return;
        if (callback !== callback.toLowerCase()) offenders.push(`${where} is not lowercase`);
        else if (SANCTIONED.has(callback)) sanctioned += 1;
        else if (NATIVE.has(callback)) {
          if (!/Attributes\["on/.test(type)) offenders.push(`${where} reuses a native name: ${type}`);
        } else if (!/^on[a-z]*change$/.test(callback) && !DISCRETE.has(callback)) {
          offenders.push(`${where} is outside the dialect`);
        }
      });
    }
    expect(offenders).toEqual([]);
    expect(sanctioned).toBeGreaterThan(0);
  });

  it("keeps every renamed callback only as a documented, deprecated alias", () => {
    const aliases = files.flatMap(({ name, text }) => {
      const lines = text.split("\n");
      return lines.flatMap((line, index) => {
        const match = /^\s+(on[A-Za-z]*[A-Z][A-Za-z]*|onvalueschange)\?:/.exec(line);
        if (!match) return [];
        return /@deprecated/.test(lines[index - 1] ?? "") ? [] : [`${name}: ${match[1]}`];
      });
    });
    expect(aliases).toEqual([]);
  });
});
