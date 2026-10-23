import { describe, expect, it } from "vitest";
import { breakdownItems } from "../src/BreakdownBar.svelte";
import { metricStatistics } from "../src/metric";
import { formatTimeTick, timeSeriesAreas, timeSeriesBars, timeSeriesDatasets, timeSeriesDateTicks } from "../src/time-series";

describe("summary and detail measurements", () => {
  it("stacks signed area contributions and leaves incomplete timestamps as gaps", () => {
    const datasets = timeSeriesDatasets([
      { id: "a", label: "A", data: [{ x: 1, y: 2 }, { x: 2, y: 3 }, { x: 3, y: -2 }] },
      { id: "b", label: "B", data: [{ x: 1, y: 5 }, { x: 3, y: -3 }] },
    ], "Requests", 1);
    const areas = timeSeriesAreas(datasets, [1, 2, 3]);
    expect(areas[1]?.data[0]).toEqual({ x: 1, y: 2, y1: 7, defined: true });
    expect(areas[1]?.data[2]).toEqual({ x: 3, y: -2, y1: -5, defined: true });
    expect(areas.every(area => area.data[1]?.defined === false)).toBe(true);
    expect(timeSeriesAreas(datasets.slice(0, 1), [1, 2, 3])[0]?.data[1]).toEqual({ x: 2, y: 0, y1: 3, defined: true });
  });
  it("labels Date axes with a bounded selection of actual timestamps while preserving numeric coordinates", () => {
    const dates = Array.from({ length: 30 }, (_, index) => new Date(Date.UTC(2026, 9, index + 1)));
    const ticks = timeSeriesDateTicks(dates, 4)!;
    expect(ticks).toHaveLength(4);
    expect(ticks[0]).toBe(+dates[0]!);
    expect(ticks.at(-1)).toBe(+dates.at(-1)!);
    expect(ticks.every(tick => dates.some(date => +date === tick))).toBe(true);
    expect(formatTimeTick(ticks[0]!, dates)).toBe("10-01");
    expect(timeSeriesDateTicks([0, 1, 2], 4)).toBeUndefined();
    const minutes = [new Date("2026-10-01T12:00:00Z"), new Date("2026-10-01T12:10:00Z")];
    expect(formatTimeTick(minutes[1]!, minutes)).toBe("12:10");
    expect(timeSeriesDateTicks([], 4)).toBeUndefined();
  });
  it("uses observed samples for statistics and leaves missing measurements unavailable", () => {
    const resolved = timeSeriesDatasets([
      { id: "a", label: "Compute", data: [{ x: 1, y: 2 }, { x: 4, y: 8 }, { x: 3, y: NaN }] },
      { id: "b", label: "Storage", data: [] },
    ], "Spend", 1);
    const [present, missing] = metricStatistics(resolved);
    expect(present).toMatchObject({ count: 2, min: 2, max: 8, average: 5, total: 10, last: 8 });
    expect(missing).toMatchObject({ count: 0, min: null, max: null, average: null, total: null, last: null });
  });

  it("stacks positive and negative contributions independently at the same timestamp", () => {
    const resolved = timeSeriesDatasets([
      { id: "a", label: "A", data: [{ x: 1, y: 2 }, { x: 2, y: -3 }] },
      { id: "b", label: "B", data: [{ x: 1, y: 5 }, { x: 2, y: 4 }] },
      { id: "c", label: "C", data: [{ x: 2, y: -2 }] },
    ], "Usage", 1);
    const bars = timeSeriesBars(resolved);
    expect(bars[1]!.data[0]).toMatchObject({ y: 2, y1: 7 });
    expect(bars[1]!.data[1]).toMatchObject({ y: 0, y1: 4 });
    expect(bars[2]!.data[0]).toMatchObject({ y: -3, y1: -5 });
    expect(bars[2]!.data).toHaveLength(1);
    for (const series of bars) for (const bar of series.data) expect(bar.x1).toBeGreaterThan(bar.x);
  });

  it("keeps a single temporal column finite and puts a restored dataset back on its own colour", () => {
    const resolved = timeSeriesDatasets([{ id: "a", label: "Compute", series: 4, data: [{ x: new Date("2026-10-16T00:00:00Z"), y: 9 }] }], "Spend", 1);
    expect(timeSeriesBars(resolved)[0]!.paint).toBe("var(--reddb-color-series-4)");
    expect(Object.values(timeSeriesBars(resolved)[0]!.data[0]!).every(Number.isFinite)).toBe(true);
    expect(timeSeriesBars([])).toEqual([]);
  });
  it("retains temporal column widths when a hidden series owns otherwise missing timestamps", () => {
    const resolved = timeSeriesDatasets([
      { id: "a", label: "A", data: [{ x: 1, y: 2 }, { x: 3, y: 4 }] },
      { id: "b", label: "B", data: [{ x: 2, y: 5 }] },
    ], "Usage", 1);
    const full = timeSeriesBars(resolved)[0]!.data;
    const hidden = timeSeriesBars([resolved[0]!], [1, 2, 3])[0]!.data;
    expect(hidden).toEqual(full);
  });
});

describe("part-to-whole contributions", () => {
  it("retains the exact denominator, explicit colour and zero-value groups", () => {
    const items = breakdownItems([{ id: "a", label: "A", value: 79, series: 4 }, { id: "b", label: "B", value: 21, series: 2 }, { id: "c", label: "C", value: 0 }]);
    expect(items.map(item => item.fraction)).toEqual([0.79, 0.21, 0]);
    expect(items[0]!.paint).toBe("var(--reddb-color-series-4)");
    expect(breakdownItems([{ id: "a", label: "A", value: 0 }])[0]!.fraction).toBe(0);
    expect(breakdownItems([])).toEqual([]);
  });
  it("rejects values that cannot represent a contribution and ambiguous identities", () => {
    for (const value of [-1, NaN, Infinity]) expect(() => breakdownItems([{ id: "a", label: "A", value }])).toThrow(/finite and non-negative/);
    expect(() => breakdownItems([{ id: "a", label: "A", value: 1 }, { id: "a", label: "A", value: 1 }])).toThrow(/duplicate group id/);
  });
});
