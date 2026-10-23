// Prose: rendered CommonMark and GFM on the Theme's type roles (ADR 0025,
// ADR 0027 "Documents: Prose and print").
//
// What is pinned here is the contract that does not need a browser: every
// element of the CommonMark and GFM set has a rule, each rule reaches its
// element through a zero-specificity `:where(&)` wrapper (so a Kit component
// inside a document keeps its own look), the headings map onto roles and not
// sizes, links are ink with a persistent underline, rhythm comes from the
// layout tier, and the component selects no appearance axis. The rendered
// result — markers, tables, print — is the showcase browser test's.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { PROSE_ELEMENTS, PROSE_MEASURES, Prose, prose } from "@reddb-io/design-system/base";
import { SRC_DIR } from "../tools/paths";
import { classes, render, rendered } from "./mount";

const html = (markup: string) => createRawSnippet(() => ({ render: () => `<div>${markup}</div>` }));

/** Every class Prose wears by default, split on whitespace. */
const base = prose().split(/\s+/);

/** The utilities a `[:where(&)_<selector>]:` variant applies to its element. */
function rulesFor(selector: string): string[] {
  const prefix = `[:where(&)_${selector}]:`;
  return base.filter((name) => name.startsWith(prefix)).map((name) => name.slice(prefix.length));
}

describe("the Base Prose", () => {
  it("offers DS-named measures and document elements", () => {
    expect([...PROSE_MEASURES]).toEqual(["prose", "content", "full"]);
    expect([...PROSE_ELEMENTS]).toEqual(["div", "article", "section"]);
  });

  it("renders its children under a data-prose root at the reading measure", () => {
    const root = rendered(render(Prose, { children: html("<p>Copy</p>") }));
    expect(root.tagName).toBe("DIV");
    expect(root.hasAttribute("data-prose")).toBe(true);
    expect(root.getAttribute("data-prose-measure")).toBe("prose");
    expect(classes(root)).toContain("max-w-prose");
    expect(classes(root)).toContain("text-body");
    expect(classes(root)).toContain("text-foreground");
    expect(root.querySelector("p")?.textContent).toBe("Copy");
  });

  it("takes its measure and element from props, never from an axis", () => {
    for (const measure of PROSE_MEASURES) {
      const root = rendered(render(Prose, { measure, as: "article", children: html("<p>x</p>") }));
      expect(root.tagName).toBe("ARTICLE");
      expect(root.getAttribute("data-prose-measure")).toBe(measure);
      for (const attribute of ["data-theme", "data-color-scheme", "data-density"]) {
        expect(root.hasAttribute(attribute), attribute).toBe(false);
      }
    }
    expect(classes(rendered(render(Prose, { measure: "full", children: html("") })))).toContain("max-w-none");
  });

  it("tints alternate table rows only when striped", () => {
    const stripe = "[:where(&)_tbody_tr:nth-child(even)]:bg-muted";
    expect(classes(rendered(render(Prose, { children: html("") })))).not.toContain(stripe);
    expect(classes(rendered(render(Prose, { striped: true, children: html("") })))).toContain(stripe);
  });

  it("has a rule for every CommonMark and GFM element", () => {
    const elements = [
      "h1", "h2", "h3", "h4", "h5", "h6", "a", "code", "kbd", "mark", "ul", "ol", "dt", "dd",
      "blockquote", "pre", "hr", "img", "figcaption", "details", "summary", "table", "caption", "th", "td",
    ];
    const selectors = new Set(
      base.flatMap((name) => /^\[:where\(&\)_(.+?)\]:/.exec(name)?.[1] ?? []),
    );
    const covered = (element: string) =>
      [...selectors].some((selector) => new RegExp(`(^|[(,_])${element}(?=$|[),_:+>\\[])`).test(selector));
    expect(elements.filter((element) => !covered(element))).toEqual([]);
    // Task items, footnotes, strikethrough and alignment: the GFM extensions.
    expect(rulesFor("li:has(>input[type=checkbox])")).toContain("list-none");
    expect(rulesFor("li>input[type=checkbox]")).toContain("accent-primary");
    expect(rulesFor("[data-footnotes]")).toContain("text-caption");
    expect(rulesFor(":is(del,s)")).toContain("text-ink-muted");
    expect(rulesFor("[align=center]")).toContain("text-center");
    expect(rulesFor("[align=right]")).toContain("text-end");
  });

  it("styles descendants at zero specificity, so a Kit component inside keeps its own classes", () => {
    const descendant = base.filter((name) => name.startsWith("["));
    expect(descendant.length).toBeGreaterThan(100);
    expect(descendant.filter((name) => !name.startsWith("[:where(&)"))).toEqual([]);
  });

  it("maps the outline onto the Theme's type roles, never onto a size step", () => {
    expect(rulesFor("h1")).toContain("text-title");
    expect(rulesFor("h2")).toContain("text-heading");
    expect(rulesFor("h3")).toContain("text-heading");
    expect(rulesFor("h4")).toEqual(expect.arrayContaining(["text-body", "font-bold"]));
    expect(rulesFor("h5")).toEqual(expect.arrayContaining(["text-body", "font-medium"]));
    expect(rulesFor("h6")).toEqual(expect.arrayContaining(["text-eyebrow", "text-ink-muted"]));
    expect(rulesFor("figcaption")).toContain("text-caption");
    const sizes = base.filter((name) => /:text-(?:xs|sm|base|lg|\d*xl|\[)/.test(name));
    expect(sizes).toEqual([]);
  });

  it("draws links in ink with a persistent underline and the ink focus outline (WCAG 1.4.1, ADR 0023)", () => {
    expect(rulesFor("a")).toEqual(expect.arrayContaining(["text-foreground", "underline"]));
    expect(rulesFor("a:focus-visible")).toEqual(
      expect.arrayContaining(["outline-2", "outline-offset-2", "outline-focus"]),
    );
    expect(base.some((name) => /_a[^\]]*\]:(?:text-primary|no-underline)/.test(name))).toBe(false);
  });

  it("keeps visible list markers at every depth", () => {
    expect(rulesFor("ul")).toContain("list-disc");
    expect(rulesFor("ul_ul")).toContain("list-[circle]");
    expect(rulesFor("ol")).toContain("list-decimal");
    expect(rulesFor("ol_ol")).toContain("list-[lower-alpha]");
    expect(rulesFor("li::marker")).toContain("text-ink-muted");
  });

  it("takes vertical rhythm from Density's layout tier (ADR 0024)", () => {
    expect(base.filter((name) => name.startsWith("[:where(&)>*+*]:"))).toEqual([
      "[:where(&)>*+*]:mt-[var(--reddb-spatial-layout-gap-sm)]",
    ]);
    expect(rulesFor(":is(h1,h2)")).toContain("mt-[var(--reddb-spatial-layout-gap-lg)]");
    const vertical = base.filter((name) => /:(?:mt|mb|my|pt|pb|py)-/.test(name));
    // Block spacing is a Density role; only an inline run's own padding is
    // typographic (`em`), because it hugs a glyph rather than separating blocks.
    expect(
      vertical.filter((name) => !/(?:-\[var\(--reddb-spatial-[a-z-]+\)\]|-0|-\[\d*\.?\d+em\])$/.test(name)),
    ).toEqual([]);
  });

  it("is built from tv() descendant variants: no <style> block and no typography plugin", () => {
    const source = readFileSync(join(SRC_DIR, "Prose.svelte"), "utf8");
    expect(source).not.toMatch(/<style\b/);
    expect(readFileSync(join(SRC_DIR, "prose.variants.ts"), "utf8")).not.toMatch(/@tailwindcss\/typography/);
  });
});
