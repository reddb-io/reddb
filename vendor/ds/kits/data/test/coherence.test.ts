// The coherence rules wave 3b moved the Data Kit onto, read off its source.
//
//   - One tone vocabulary (ADR 0026): KpiCard takes Base's TONES; `brand`
//     survives one release as a deprecated value.
//   - Focus is ink (ADR 0023): every focus indicator a Data class draws is the
//     `focus` role as an outline — grid cells and graph nodes included —
//     never the Brand red, `ring-foreground` or any other ring. A grid cell
//     insets its outline (`-outline-offset-2`), because the scroll region
//     would clip one drawn outside it.
//   - Container-first (ADR 0021): no Data component steps on the viewport;
//     DocumentViewer steps its panes on its own named container.
//   - Layout tier (ADR 0024): Table spaces its toolbar, grid and pagination,
//     and its filter fields, from the layout tier.
//   - One event dialect (ADR 0026): every public callback is lowercase —
//     `on<state>change`, `onvaluechange`, a discrete `on<noun>`, or the
//     sanctioned `onselect` for item activation. A camelCase name survives
//     only as a `@deprecated` alias.
//   - Type roles (ADR 0025): the Data Kit's type ledger is paid in full.

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { TONES } from "@reddb-io/design-system/base";
import { flushSync, mount, unmount } from "svelte";
import { afterEach, describe, expect, it } from "vitest";
import DocumentViewer from "../src/DocumentViewer.svelte";
import Graph from "../src/Graph.svelte";
import KpiCard, { DEPRECATED_KPI_CARD_TONES, KPI_CARD_TONES } from "../src/KpiCard.svelte";
import Table from "../src/Table.svelte";

const KIT = join(import.meta.dirname, "..");
const SOURCE = join(KIT, "src");
const files = readdirSync(SOURCE)
  .filter((name) => /\.(?:ts|svelte)$/.test(name))
  .map((name) => ({ name, text: readFileSync(join(SOURCE, name), "utf8") }));

/** Every class-like token in a file's string literals and class attributes. */
function tokens(text: string): string[] {
  return [...text.matchAll(/["'`]([^"'`\n]*)["'`]/g)].flatMap(([, literal]) => literal!.split(/\s+/));
}

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];
function render(component: Parameters<typeof mount>[0], props: Record<string, unknown>): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(component, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}
afterEach(() => {
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

describe("the Data Kit's tone vocabulary", () => {
  it("is Base's TONES, with brand kept one release as a deprecated value", () => {
    expect(KPI_CARD_TONES).toEqual(TONES);
    expect(DEPRECATED_KPI_CARD_TONES).toEqual(["brand"]);
    for (const tone of TONES) {
      const card = render(KpiCard, { label: tone, value: 1, tone }).querySelector<HTMLElement>("[data-kpi-card]")!;
      expect(card.dataset.tone).toBe(tone);
      // info is feedback-info here as on every other component.
      if (tone !== "neutral") expect(card.className).toContain(`--reddb-color-feedback-${tone}-surface`);
    }
  });
});

describe("the Data Kit's focus indicators", () => {
  it("draw only the ink focus role as an outline, never a ring", () => {
    const offenders = files.flatMap(({ name, text }) =>
      tokens(text)
        .filter((token) => /focus(?:-visible|-within)?:/.test(token))
        .filter((token) => /:(?:ring-|outline-(?!2\b|offset-2\b|focus\b))/.test(token))
        .map((token) => `${name}: ${token}`),
    );
    expect(offenders).toEqual([]);

    for (const { name, text } of files) {
      const styles = tokens(text).some((token) => /focus(?:-visible|-within):(?:-?outline|ring)/.test(token));
      if (!styles) continue;
      const role = tokens(text).filter((token) => /focus(?:-visible|-within):outline-focus$/.test(token));
      expect(role.length, `${name} draws the focus role`).toBeGreaterThan(0);
    }
  });

  it("outlines grid cells inside the scroll region and graph nodes and edges with the role", () => {
    const table = render(Table, {
      caption: "Regions",
      columns: [{ key: "region", header: "Region", sortable: true }],
      rows: [{ region: "Frankfurt" }],
    });
    const cell = table.querySelector<HTMLElement>("[data-table-cell]")!;
    for (const token of ["focus-visible:outline-2", "focus-visible:-outline-offset-2", "focus-visible:outline-focus"]) {
      expect(cell.classList.contains(token), token).toBe(true);
    }
    for (const element of [
      table.querySelector<HTMLElement>("[data-table-scroll]")!,
      table.querySelector<HTMLElement>("thead button")!,
      table.querySelector<HTMLElement>("summary")!,
    ]) {
      expect(element.classList.contains("focus-visible:outline-focus")).toBe(true);
      expect(element.classList.contains("focus-visible:outline-offset-2")).toBe(true);
    }

    const graph = render(Graph, {
      data: { nodes: [{ id: "a" }, { id: "b" }], edges: [{ id: "ab", source: "a", target: "b" }] },
      renderer: "svg",
    });
    for (const element of [
      graph.querySelector<SVGElement>('[data-node-id="a"]')!,
      graph.querySelector<SVGElement>('[data-edge-id="ab"]')!,
    ]) {
      expect(element.classList.contains("focus-visible:outline-focus")).toBe(true);
    }
  });
});

describe("the Data Kit's responsive steps", () => {
  it("never query the viewport: every step is a container step", () => {
    const viewport = /^(?:[a-z-]+:)*(?:max-)?(?:sm|md|lg|xl|2xl):/;
    const offenders = files.flatMap(({ name, text }) =>
      tokens(text).filter((token) => viewport.test(token)).map((token) => `${name}: ${token}`),
    );
    expect(offenders).toEqual([]);
  });

  it("steps DocumentViewer's panes on its own named container", () => {
    const root = render(DocumentViewer, { documents: [{ id: "one", title: "One" }] });
    const viewer = root.querySelector<HTMLElement>("[data-document-viewer]")!;
    const panes = viewer.querySelector<HTMLElement>("[data-document-panes]")!;
    expect(viewer.classList.contains("@container/document-viewer")).toBe(true);
    expect(panes.className).toContain("@2xl/document-viewer:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]");
  });
});

describe("the Data Kit's layout rhythm", () => {
  it("spaces Table's blocks and filter fields from the layout tier", () => {
    const root = render(Table, {
      caption: "Regions",
      columns: [{ key: "region", header: "Region", filterable: true }],
      rows: [{ region: "Frankfurt" }],
    });
    expect(root.querySelector("[data-data-table]")!.classList.contains("gap-[var(--reddb-spatial-layout-gap-sm)]")).toBe(true);
    expect(root.querySelector("[data-table-tools]")!.classList.contains("gap-[var(--reddb-spatial-layout-gap-sm)]")).toBe(true);
  });
});

describe("the Data Kit's event dialect", () => {
  /** Discrete Data events: a lowercase noun, never a native event name. */
  const DISCRETE = new Set(["onrename", "onreorder", "oninteraction"]);
  /** The sanctioned exception (ADR 0026): item activation — a row, a document. */
  const SANCTIONED = new Set(["onselect"]);

  it("names every public callback in the one dialect; camelCase survives only as a deprecated alias", () => {
    const offenders: string[] = [];
    let deprecated = 0;
    let sanctioned = 0;
    for (const { name, text } of files) {
      const lines = text.split("\n");
      lines.forEach((line, index) => {
        const match = /^\s+(on[A-Za-z]+)\?:/.exec(line);
        if (!match) return;
        const callback = match[1]!;
        const where = `${name}: ${callback}`;
        if (/@deprecated/.test(lines[index - 1] ?? "")) {
          deprecated += 1;
          return;
        }
        if (callback !== callback.toLowerCase()) offenders.push(`${where} is not lowercase`);
        else if (SANCTIONED.has(callback)) sanctioned += 1;
        else if (!/^on[a-z]+change$/.test(callback) && !DISCRETE.has(callback)) offenders.push(`${where} is outside the dialect`);
      });
    }
    expect(offenders).toEqual([]);
    expect(sanctioned).toBe(2);
    expect(deprecated).toBe(19);
  });

  it("never passes a deprecated name to a parent component", () => {
    const deprecatedParentNames = /\s(?:intent|feedback|status|expanded|onchange)=\{?["'(]/;
    for (const { name, text } of files.filter((file) => file.name.endsWith(".svelte"))) {
      for (const element of text.matchAll(/<([A-Z][A-Za-z]*)\b[^>]*>/gs)) {
        expect(deprecatedParentNames.test(element[0]), `${name}: <${element[1]}>`).toBe(false);
      }
    }
  });
});

describe("the Data Kit's type ledger", () => {
  it("is paid in full (ADR 0025)", () => {
    const ledger = JSON.parse(readFileSync(join(KIT, "type-ledger.json"), "utf8")) as { debts: object };
    expect(ledger.debts).toEqual({});
  });
});
