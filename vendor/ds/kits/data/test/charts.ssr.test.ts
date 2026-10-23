/**
 * @vitest-environment node
 */

import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import Heatmap from "../src/Heatmap.svelte";
import Histogram from "../src/Histogram.svelte";
import KpiCard from "../src/KpiCard.svelte";
import Sparkline from "../src/Sparkline.svelte";
import TimeSeries from "../src/TimeSeries.svelte";

const points = [
  { x: 1, y: 12 },
  { x: 2, y: 18 },
  { x: 3, y: 15 },
] as const;

const chartFixtures = [
  [TimeSeries, { data: points, label: "Requests", width: 480, height: 240 }],
  [
    TimeSeries,
    { data: points, label: "Storage", variant: "area", width: 480, height: 240 },
  ],
  [Histogram, { values: [1, 2, 2, 3, 8], label: "Latency", width: 480, height: 240 }],
  [Sparkline, { values: [3, 5, 4, 8], label: "Trend", width: 160, height: 48 }],
  [
    Heatmap,
    {
      data: [
        { date: new Date(2026, 0, 1), value: 1 },
        { date: new Date(2026, 0, 2), value: 3 },
      ],
      start: new Date(2026, 0, 1),
      end: new Date(2026, 0, 8),
      label: "Activity",
      width: 480,
      height: 112,
    },
  ],
] as const;

describe("the chart prerender contract", () => {
  it("prerenders each chart as a named, sized placeholder and leaves LayerChart to the client (wave 5D)", () => {
    for (const [component, props] of chartFixtures) {
      const { body } = render(component, { props });

      expect(body).toContain("data-ds-chart");
      expect(body).toContain('role="img"');
      expect(body).toContain(`aria-label="${props.label}"`);
      expect(body).toContain("data-chart-placeholder");
      expect(body).toContain(`height: ${props.height}px`);
      // The engine is a dynamic import: nothing of LayerChart is in the server HTML.
      expect(body).not.toContain("lc-root-container");
      expect(body).not.toContain("<svg");
      expect(body).not.toMatch(/undefined|NaN/);
    }
  });

  it("sizes the placeholder from the Density when no height is given", () => {
    const { body } = render(TimeSeries, { props: { data: points, label: "Requests" } });
    expect(body).toContain("height: calc(var(--reddb-spatial-control-height-md) * 10)");
  });

  it("prerenders KpiCard without a LayerChart boundary", () => {
    const { body } = render(KpiCard, {
      props: { label: "Monthly revenue", value: "$48k", delta: 12.5 },
    });

    expect(body).toContain("data-kpi-card");
    expect(body).toContain("Monthly revenue");
    expect(body).toContain("$48k");
    expect(body).not.toContain("lc-root-container");
  });
});
