import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { APPEARANCE_THEMES, BASE_THEME, COLOR_SCHEMES } from "../../../packages/theme/src/appearance";
import { appearanceThemeCssPath, colorSchemeCssPath } from "../../../packages/theme/src/paths";
import {
  cascadeFor,
  over,
  parseStylesheets,
  resolveColor,
} from "../../../packages/theme/test/support/cascade";
import { formatChartNumber, fractionDigitsFor, heatmapStep } from "../src/chart";
import { preloadChartEngines } from "../src/chart-engines";
import Heatmap from "../src/Heatmap.svelte";
import Histogram, { histogramBins } from "../src/Histogram.svelte";
import KpiCard from "../src/KpiCard.svelte";
import Sparkline from "../src/Sparkline.svelte";
import TimeSeries from "../src/TimeSeries.svelte";
import KpiSparklineConsumer from "./fixtures/KpiSparklineConsumer.svelte";

// Rendering LayerChart in jsdom is slow on a shared machine; the budget is for
// the engine, not for anything these assertions wait on.
const RENDER_TIMEOUT = 60_000;

// The engines are dynamic imports (wave 5D); loaded once up front, a chart
// mounted without IntersectionObserver (jsdom) draws within the mount's flush.
beforeAll(() => preloadChartEngines(), RENDER_TIMEOUT);

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(
  component: Parameters<typeof mount>[0],
  props: Record<string, unknown>,
): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(component, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

afterEach(() => {
  vi.restoreAllMocks();
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

const points = [
  { x: 1, y: 12 },
  { x: 2, y: 18 },
  { x: 3, y: 15 },
] as const;

// One local calendar week: four distinct intensities, an explicit zero and two
// days with no data at all.
const week = { start: new Date(2026, 0, 1), end: new Date(2026, 0, 8) } as const;
const heatmap = [
  { date: new Date(2026, 0, 1), value: 1 },
  { date: new Date(2026, 0, 2), value: 2 },
  { date: new Date(2026, 0, 3), value: 3 },
  { date: new Date(2026, 0, 4), value: 4 },
  { date: new Date(2026, 0, 5), value: 0 },
] as const;
const heatmapByDay = [1, 2, 3, 4, 0, undefined, undefined] as const;

const latency = [18, 21, 22, 24, 25, 25, 27, 29, 31, 33, 35, 39, 41, 44, 48, 52];

const chartFixtures = [
  [TimeSeries, { data: points, label: "Requests", width: 480, height: 240 }],
  [
    TimeSeries,
    { data: points, label: "Storage", variant: "area", width: 480, height: 240 },
  ],
  [Histogram, { values: [1, 2, 2, 3, 8], label: "Latency", width: 480, height: 240 }],
  [Sparkline, { values: [3, 5, 4, 8], label: "Trend", width: 160, height: 48 }],
  [Heatmap, { data: heatmap, ...week, label: "Activity", width: 480, height: 112 }],
] as const;

const figureOf = (root: HTMLElement): HTMLElement => root.querySelector<HTMLElement>("[data-ds-chart]")!;

// ---------------------------------------------------------------------------
// Painted output: what a chart's fills and strokes resolve to under a real
// appearance, read from the Theme Layer's generated artifacts (the same
// cascade the KpiCard contrast suite resolves) plus the custom properties the
// chart's own figure bridges for LayerChart.

const composition = parseStylesheets([
  readFileSync(join(import.meta.dirname, "..", "..", "..", "packages", "tokens", "dist", "tokens.css"), "utf8"),
  readFileSync(appearanceThemeCssPath(BASE_THEME), "utf8"),
  ...APPEARANCE_THEMES.map((theme) => readFileSync(appearanceThemeCssPath(theme), "utf8")),
  ...COLOR_SCHEMES.map((scheme) => readFileSync(colorSchemeCssPath(scheme), "utf8")),
]);

const APPEARANCES = APPEARANCE_THEMES.flatMap((theme) =>
  COLOR_SCHEMES.map((scheme) => ({ theme: theme.name, scheme: scheme.name })),
);

/** `[--name:value]` classes on `element`, as the custom properties they declare. */
function bridgesOf(element: HTMLElement): Map<string, string> {
  const declared = new Map<string, string>();
  for (const raw of element.className.split(/\s+/)) {
    const match = /^\[(--[a-z0-9-]+):(.+)\]$/i.exec(raw);
    if (match) declared.set(match[1]!, match[2]!.replace(/_/g, " "));
  }
  return declared;
}

/** Resolve a paint string on `figure` to the opaque colour it shows over the ground. */
function painter(figure: HTMLElement, appearance: { theme: string; scheme: string }) {
  const cascade = new Map(
    cascadeFor(composition, [{ "data-theme": appearance.theme, "data-color-scheme": appearance.scheme }]),
  );
  for (const [property, value] of bridgesOf(figure)) cascade.set(property, value);
  const ground = resolveColor(cascade, "--reddb-color-background").hex;
  const paint = (value: string): string => {
    cascade.set("--painted", value);
    return over(resolveColor(cascade, "--painted"), ground);
  };
  return { paint, ground, token: (name: string) => paint(`var(${name})`) };
}

/** Every fill and stroke the chart's SVG actually paints. */
function paintsOf(figure: HTMLElement): string[] {
  const paints: string[] = [];
  for (const element of figure.querySelectorAll("svg [fill], svg [stroke]")) {
    const fill = element.getAttribute("fill");
    const stroke = element.getAttribute("stroke");
    if (fill && !/^(none|transparent)$/.test(fill)) paints.push(fill);
    if (stroke && stroke !== "none" && element.getAttribute("stroke-width") !== "0") paints.push(stroke);
  }
  return paints;
}

function channels(hex: string): number[] {
  return [1, 3, 5].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16));
}

function distance(left: string, right: string): number {
  const [a, b] = [channels(left), channels(right)];
  return Math.hypot(a[0]! - b[0]!, a[1]! - b[1]!, a[2]! - b[2]!);
}

/** The tick labels of one axis, in drawing order. */
function tickLabels(figure: HTMLElement, placement: "left" | "bottom"): string[] {
  return [...figure.querySelectorAll(`.lc-axis.placement-${placement} .lc-axis-tick-label`)].map(
    (label) => label.textContent?.trim() ?? "",
  );
}

/** The largest y any vertex of an SVG path reaches: its lowest drawn point. */
function lowestPoint(path: Element): number {
  const numbers = [...(path.getAttribute("d") ?? "").matchAll(/-?\d+(?:\.\d+)?/g)].map(Number);
  return Math.max(...numbers.filter((_, index) => index % 2 === 1));
}

describe("Histogram bins", () => {
  it("bins raw values without mutating them and keeps the final maximum in range", () => {
    const values = [0, 2, 4, 6, 8];

    expect(histogramBins(values, 2)).toEqual([
      { x0: 0, x1: 4, label: "0–4", count: 2 },
      { x0: 4, x1: 8, label: "4–8", count: 3 },
    ]);
    expect(values).toEqual([0, 2, 4, 6, 8]);
    expect(histogramBins([], 4)).toEqual([]);
  });

  it("labels each bin with formatted edges, never a raw float", () => {
    expect(histogramBins(latency, 6).map((bin) => bin.label)).toEqual([
      "18–23.7",
      "23.7–29.3",
      "29.3–35",
      "35–40.7",
      "40.7–46.3",
      "46.3–52",
    ]);
    expect(histogramBins(latency, 6).map((bin) => bin.count)).toEqual([3, 5, 2, 2, 2, 2]);
    expect(histogramBins([0, 0.1, 0.25, 0.5], 4).map((bin) => bin.label)).toEqual([
      "0–0.13",
      "0.13–0.25",
      "0.25–0.38",
      "0.38–0.5",
    ]);
    expect(histogramBins(latency, 2, (edge) => `${Math.round(edge)} ms`).map((bin) => bin.label)).toEqual([
      "18 ms–35 ms",
      "35 ms–52 ms",
    ]);
  });

  it("formats numbers without a locale, so a prerendered label hydrates unchanged", () => {
    expect(formatChartNumber(23.666666666666668, 1)).toBe("23.7");
    expect(formatChartNumber(12000)).toBe("12000");
    expect(formatChartNumber(-0.0001)).toBe("0");
    expect(fractionDigitsFor(5.67)).toBe(1);
    expect(fractionDigitsFor(10)).toBe(0);
    expect(fractionDigitsFor(0.25)).toBe(2);
  });
});

describe("the LayerChart-powered chart contracts", () => {
  it("draws smooth default curves and preserves explicit linear segments", () => {
    for (const component of [Sparkline, TimeSeries]) {
      const content = component === Sparkline ? { values: [3, 8, 4, 9] } : { data: [{ x: 0, y: 3 }, { x: 1, y: 8 }, { x: 2, y: 4 }, { x: 3, y: 9 }] };
      const smooth = render(component, { ...content, width: 240, height: 80 });
      const linear = render(component, { ...content, width: 240, height: 80, curve: "linear" });
      const paths = (root: HTMLElement) => [...root.querySelectorAll('path[stroke="var(--color-primary)"]')].map(path => path.getAttribute("d") ?? "").join(" ");
      expect(paths(smooth)).toMatch(/C/);
      expect(paths(linear)).not.toMatch(/C/);
      expect(paths(linear)).toMatch(/L/);
    }
  });

  it("draws area and column sparklines with finite geometry on both sides of zero", () => {
    const area = render(Sparkline, { values: [-4, 8, -2, 6], variant: "area", width: 200, height: 64 });
    expect(area.querySelector("[data-sparkline-area]")?.getAttribute("d")).toMatch(/Z/);
    const bars = render(Sparkline, { values: [-4, 8, -2, 6], variant: "bar", width: 200, height: 64 });
    const columns = [...bars.querySelectorAll("[data-sparkline-column]")];
    expect(columns).toHaveLength(4);
    for (const column of columns) {
      expect(Number(column.getAttribute("width"))).toBeGreaterThan(0);
      expect(Number(column.getAttribute("height"))).toBeGreaterThan(0);
    }
    expect(Number(columns[0]!.getAttribute("y"))).toBeGreaterThan(Number(columns[1]!.getAttribute("y")));
    expect(bars.querySelector(".lc-axis")).toBeNull();
  });
  it("keeps empty, zero, single and unavailable sparkline observations finite", () => {
    for (const variant of ["line", "area", "bar"]) for (const values of [[], [0], [5], [-3], [NaN, Infinity]]) {
      const root = render(Sparkline, { values, variant, width: 160, height: 48 });
      for (const mark of root.querySelectorAll("path,rect")) for (const name of ["d", "x", "y", "width", "height"]) expect(mark.getAttribute(name) ?? "").not.toMatch(/NaN|Infinity/);
    }
  });

  it("draws stacked area bands through LayerChart instead of overlapping fills", () => {
    const root = render(TimeSeries, { label: "Requests", width: 480, height: 240, variant: "stacked-area", data: [
      { id: "a", label: "A", data: [{ x: 1, y: 2 }, { x: 2, y: 4 }, { x: 3, y: 3 }] },
      { id: "b", label: "B", data: [{ x: 1, y: 5 }, { x: 2, y: 6 }, { x: 3, y: 4 }] },
    ] });
    expect(root.querySelectorAll("[data-chart-area]")).toHaveLength(2);
    expect([...root.querySelectorAll("[data-chart-area]")].every(path => path.getAttribute("d")?.includes("Z"))).toBe(true);
    expect(tickLabels(figureOf(root), "left")).toContain("10");
  });
  it(
    "routes every chart boundary's spacing through inherited Density",
    () => {
      for (const [component, props] of chartFixtures) {
        const chart = figureOf(render(component, props));

        expect(chart.className).toContain("p-[var(--reddb-spatial-inset-sm)]");
        expect(chart.hasAttribute("data-density")).toBe(false);
        expect(chart.querySelector("[data-chart-rhythm-probe]")?.getAttribute("class")).toContain(
          "h-[var(--reddb-spatial-control-height-md)]",
        );
      }
    },
    RENDER_TIMEOUT,
  );

  it(
    "renders every chart with an accessible DS-owned boundary and legible axis text",
    () => {
      for (const [component, props] of chartFixtures) {
        const chart = figureOf(render(component, props));

        expect(chart.getAttribute("role")).toBe("img");
        expect(chart.getAttribute("aria-label")).toBe(props.label);
        expect(chart.className).toContain("[&_.lc-axis-tick-label]:text-xs");
        expect(chart.className).not.toContain("surface-muted");
        expect(chart.querySelector(".lc-root-container")).not.toBeNull();
        expect(chart.textContent).not.toMatch(/undefined|NaN|\[object Object\]/);
        expect(
          [...chart.querySelectorAll("[d], [x], [y], [width], [height]")].every((element) =>
            [...element.attributes].every((attribute) => !attribute.value.includes("NaN")),
          ),
        ).toBe(true);
      }
    },
    RENDER_TIMEOUT,
  );
});

describe("painted output across application|marketing × light|dark", () => {
  it(
    "draws every measurement in a Series Role and never in the Brand red",
    () => {
      for (const [component, props] of chartFixtures) {
        const chart = figureOf(render(component, props));
        const paints = paintsOf(chart);
        expect(paints.length, `${props.label} paints nothing`).toBeGreaterThan(0);

        for (const appearance of APPEARANCES) {
          const where = `${props.label} under ${appearance.theme}/${appearance.scheme}`;
          const { paint, token } = painter(chart, appearance);
          const red = token("--reddb-color-primary");

          expect(token("--color-primary"), where).toBe(token("--reddb-color-series-1"));
          for (const value of paints) expect(paint(value), `${where}: ${value}`).not.toBe(red);
        }
      }
    },
    RENDER_TIMEOUT,
  );

  it(
    "takes the Series Role a caller names",
    () => {
      const chart = figureOf(render(TimeSeries, { data: points, label: "Errors", series: 4, width: 480, height: 240 }));

      expect(chart.dataset.chartSeries).toBe("4");
      for (const appearance of APPEARANCES) {
        const { paint, token } = painter(chart, appearance);
        const stroke = chart.querySelector("path[stroke]:not([stroke='none'])")!.getAttribute("stroke")!;
        expect(paint(stroke)).toBe(token("--reddb-color-series-4"));
      }
    },
    RENDER_TIMEOUT,
  );

  it(
    "steps distinct heatmap values into distinct fills that climb away from the ground",
    () => {
      const chart = figureOf(render(Heatmap, { data: heatmap, ...week, label: "Activity", width: 480, height: 112 }));
      const cells = [...chart.querySelectorAll("svg rect")];
      expect(cells).toHaveLength(heatmapByDay.length);

      for (const appearance of APPEARANCES) {
        const where = `${appearance.theme}/${appearance.scheme}`;
        const { paint, ground } = painter(chart, appearance);
        const painted = cells.map((cell) => paint(cell.getAttribute("fill")!));
        const byValue = new Map<number, string>();
        heatmapByDay.forEach((value, day) => byValue.set(value ?? 0, painted[day]!));

        // Nothing and zero read the same quiet surface; every value above it
        // is its own colour, and a larger value stands further off the ground.
        expect(painted[4], where).toBe(painted[5]);
        expect(new Set(byValue.values()).size, where).toBe(5);
        const climb = [0, 1, 2, 3, 4].map((value) => distance(byValue.get(value)!, ground));
        expect(climb, where).toEqual([...climb].sort((a, b) => a - b));
        expect(painted[0], where).not.toBe(ground);
      }
    },
    RENDER_TIMEOUT,
  );

  it("steps an intensity against the largest value shown", () => {
    expect([undefined, Number.NaN, -3, 0].map((value) => heatmapStep(value, 22))).toEqual([0, 0, 0, 0]);
    expect([1, 5, 6, 11, 12, 17, 22].map((value) => heatmapStep(value, 22))).toEqual([1, 1, 2, 2, 3, 4, 4]);
    expect(heatmapStep(4, 0)).toBe(0);
  });
});

describe("value axes measure from zero", () => {
  it(
    "starts every histogram at a zero baseline, so bar height is proportional to count",
    () => {
      const chart = figureOf(render(Histogram, { values: latency, bins: 6, label: "Latency", width: 480, height: 240 }));
      const bars = [...chart.querySelectorAll<SVGRectElement>(".lc-bars rect")];
      const heights = bars.map((bar) => Number(bar.getAttribute("height")));
      const bottoms = bars.map((bar) => Number(bar.getAttribute("y")) + Number(bar.getAttribute("height")));

      expect(bars).toHaveLength(6);
      expect(new Set(bottoms).size).toBe(1);
      // Counts 3, 5, 2, 2, 2, 2: against zero, a 2-count bar is 40% of the 5-count bar.
      for (const [index, count] of [3, 5, 2, 2, 2, 2].entries()) {
        expect(heights[index]! / heights[1]!).toBeCloseTo(count / 5, 5);
      }
      expect(tickLabels(chart, "left")[0]).toBe("0");
      expect(tickLabels(chart, "left").every((label) => /^\d+$/.test(label))).toBe(true);
    },
    RENDER_TIMEOUT,
  );

  it(
    "labels histogram bins with formatted edges and only as many as fit",
    () => {
      const wide = figureOf(render(Histogram, { values: latency, bins: 6, label: "Wide", width: 960, height: 240 }));
      const narrow = figureOf(render(Histogram, { values: latency, bins: 6, label: "Narrow", width: 320, height: 240 }));
      const custom = figureOf(
        render(Histogram, {
          values: latency,
          bins: 2,
          label: "Custom",
          width: 640,
          height: 240,
          format: (edge: number) => `${Math.round(edge)} ms`,
        }),
      );

      expect(tickLabels(wide, "bottom")).toEqual(histogramBins(latency, 6).map((bin) => bin.label));
      expect(tickLabels(narrow, "bottom").length).toBeLessThan(6);
      expect(tickLabels(narrow, "bottom")[0]).toBe("18–23.7");
      expect(tickLabels(custom, "bottom")).toEqual(["18 ms–35 ms", "35 ms–52 ms"]);
    },
    RENDER_TIMEOUT,
  );

  it(
    "fills an area series down to zero, while a line keeps the data's own range",
    () => {
      const area = figureOf(render(TimeSeries, { data: points, label: "Area", variant: "area", width: 480, height: 240 }));
      const line = figureOf(render(TimeSeries, { data: points, label: "Line", width: 480, height: 240 }));
      const plotBottom = Number(area.querySelector(".lc-grid-y-line")?.getAttribute("y1"));
      const spline = area.querySelector("path[stroke]:not([stroke='none'])")!;

      expect(tickLabels(area, "left")[0]).toBe("0");
      expect(lowestPoint(area.querySelector(".lc-area-path")!)).toBeCloseTo(plotBottom, 5);
      // The smallest value, 12, sits two thirds of the way up an axis to 18 —
      // not on the floor, which is where a range starting at 12 put it.
      expect(lowestPoint(spline)).toBeLessThan(plotBottom / 2);
      expect(tickLabels(line, "left")).not.toContain("0");
    },
    RENDER_TIMEOUT,
  );

  it(
    "formats the value axis through a caller's formatter",
    () => {
      const chart = figureOf(
        render(TimeSeries, {
          data: points,
          label: "Formatted",
          variant: "area",
          width: 480,
          height: 240,
          format: (value: number) => `${value} rps`,
        }),
      );

      expect(tickLabels(chart, "left").every((label) => label.endsWith(" rps"))).toBe(true);
    },
    RENDER_TIMEOUT,
  );
});

describe("chart geometry", () => {
  it(
    "keeps a caller's fixed width and otherwise fills its figure from a measured fallback",
    () => {
      for (const [component, props] of [
        [TimeSeries, { data: points, height: 240 }],
        [Histogram, { values: latency, height: 240 }],
        [Heatmap, { data: heatmap, ...week, height: 112 }],
      ] as const) {
        const fixed = figureOf(render(component, { ...props, label: "Fixed", width: 480 }));
        const fluid = figureOf(render(component, { ...props, label: "Fluid" }));

        expect(fixed.dataset.chartWidth).toBe("fixed");
        expect(fixed.querySelector("svg")?.getAttribute("width")).toBe("480");
        expect(fluid.dataset.chartWidth).toBe("fluid");
        // A fluid drawing never widens the container it is measured from.
        expect(fluid.className).toContain("contain-inline-size");
        expect(fixed.className).not.toContain("contain-inline-size");
        // jsdom performs no layout, so the unmeasured figure draws at the SSR fallback.
        expect(fluid.querySelector("svg")?.getAttribute("width")).toBe("640");
      }
    },
    RENDER_TIMEOUT,
  );

  it(
    "takes its default height from the Density's control height, read off the probe",
    () => {
      const defaults = [
        [TimeSeries, { data: points, label: "Series", width: 480 }, 10],
        [Histogram, { values: latency, label: "Bins", width: 480 }, 10],
        [Heatmap, { data: heatmap, ...week, label: "Days", width: 480 }, 5],
        [Sparkline, { values: [3, 5, 4, 8], label: "Trend" }, 1.5],
      ] as const;
      const heightOf = (root: HTMLElement) => Number(figureOf(root).querySelector("svg")?.getAttribute("height"));

      // Unmeasured, every chart draws at the comfortable stop's 2rem row.
      for (const [component, props, rows] of defaults) {
        expect(heightOf(render(component, props)), props.label).toBe(32 * rows);
      }

      // Under `tiny` the probe measures a 24px row and an 8px inset.
      const measure = HTMLElement.prototype.getBoundingClientRect;
      vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
        return this.hasAttribute("data-chart-rhythm-probe")
          ? DOMRect.fromRect({ width: 8, height: 24 })
          : measure.call(this);
      });
      for (const [component, props, rows] of defaults) {
        expect(heightOf(render(component, props)), `${props.label} under tiny`).toBe(24 * rows);
      }
      const series = figureOf(render(TimeSeries, { data: points, label: "Inset", width: 480 }));
      expect(series.querySelector(".lc-layout-svg-g")?.getAttribute("transform")).toMatch(/, 8\)$/);
    },
    RENDER_TIMEOUT,
  );
});

describe("KpiCard", () => {
  it("presents value and signed delta without importing a chart engine", () => {
    const root = render(KpiCard, {
      label: "Monthly revenue",
      value: "$48k",
      delta: 12.5,
    });
    const card = root.querySelector<HTMLElement>("[data-kpi-card]")!;

    expect(card.querySelector("[data-kpi-label]")?.textContent).toBe("Monthly revenue");
    expect(card.querySelector("[data-kpi-value]")?.textContent).toBe("$48k");
    expect(card.querySelector("[data-kpi-change]")?.textContent?.trim()).toBe(
      "+12.5%",
    );
    expect(card.dataset.deltaDirection).toBe("increase");
    expect(card.querySelector(".lc-root-container")).toBeNull();
  });

  it("accepts an optional caller-imported Sparkline", () => {
    const root = render(KpiSparklineConsumer, {});
    const card = root.querySelector<HTMLElement>("[data-kpi-card]")!;

    expect(card.querySelector("[data-kpi-sparkline]")).not.toBeNull();
    expect(card.querySelector('[data-chart-kind="sparkline"]')).not.toBeNull();
    expect(card.querySelector('[aria-label="Monthly revenue trend"]')).not.toBeNull();
  });
});


describe("named series readings", () => {
  it("names a missing reading at the shared timestamp and keeps the scale when a series is hidden", () => {
    const chart = figureOf(render(TimeSeries, {
      label: "Latency comparison", width: 480, height: 240, valueDomain: [0, 100],
      data: [
        { id: "p50", label: "P50", series: 1, data: [{ x: 1, y: 5 }, { x: 3, y: 7 }] },
        { id: "p95", label: "P95", series: 2, data: [{ x: 1, y: 15 }, { x: 2, y: 21 }] },
      ],
    }));
    expect(chart.querySelector('[data-chart-reading="p95"]')?.textContent).toContain("No sample");
    expect(chart.querySelector('[data-chart-reading="p50"]')?.textContent).toContain("7");
    const scale = tickLabels(chart, "left");
    expect(scale[0]).toBe("0");
    expect(scale.at(-1)).toBe("100");
    chart.querySelector<HTMLButtonElement>('[data-chart-legend-item="p95"]')!.click();
    flushSync();
    expect(chart.querySelector('[data-chart-line="p95"]')).toBeNull();
    expect(tickLabels(chart, "left")).toEqual(scale);
  }, RENDER_TIMEOUT);

  it("renders empty named measurements without inventing a zero sample or invalid SVG", () => {
    const chart = figureOf(render(TimeSeries, {
      label: "Empty comparison", width: 480, height: 240,
      data: [{ id: "missing", label: "Missing", data: [] }],
    }));
    expect(chart.textContent).toContain("No samples");
    expect(chart.querySelector('[data-chart-reading="missing"]')?.textContent).toContain("No sample");
    for (const element of chart.querySelectorAll("svg *")) {
      expect([...element.attributes].some((attribute) => /NaN|Infinity/.test(attribute.value))).toBe(false);
    }
  }, RENDER_TIMEOUT);
});
