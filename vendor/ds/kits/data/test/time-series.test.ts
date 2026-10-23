import { describe, expect, it } from "vitest";
import { formatTimeSample, nearestTimeIndex, timeSeriesDatasets, timeSeriesDomain, timeSeriesSamples, timeSeriesValueDomain } from "../src/time-series";

describe("comparable time series", () => {
  it("retains exact value bounds and rejects misleading area scales", () => {
    expect(timeSeriesValueDomain([0, 100], false)).toEqual([0, 100]);
    expect(timeSeriesValueDomain(undefined, false)).toBeUndefined();
    for (const domain of [[4, 4], [100, 0], [0, Infinity]] as const) {
      expect(() => timeSeriesValueDomain(domain, false)).toThrow(/finite, increasing/);
    }
    expect(() => timeSeriesValueDomain([10, 100], true)).toThrow(/include zero/);
  });
  it("builds a common time axis from uneven datasets without mutating input", () => {
    const data = [
      { id: "a", label: "P50", data: [{ x: 30, y: 3 }, { x: 10, y: 1 }], series: 4 as const },
      { id: "b", label: "P95", data: [{ x: 20, y: 7 }, { x: 30, y: 8 }] },
    ];
    const resolved = timeSeriesDatasets(data, "Latency", 1);
    expect(timeSeriesSamples(resolved)).toEqual([10, 20, 30]);
    expect(data[0]!.data[0]!.x).toBe(30);
    expect(resolved[0]!.paint).toBe("var(--reddb-color-series-4)");
    // A missing sample remains missing rather than borrowing the adjacent time.
    expect(resolved[0]!.data.find((point) => point.x === 20)).toBeUndefined();
    expect(timeSeriesDatasets([...data].reverse(), "Latency", 1)[1]!.paint).toBe(resolved[0]!.paint);
  });
  it("chooses the closest actual timestamp rather than a proportional array index", () => {
    const samples = [0, 1, 100, 101];
    expect([-30, 0.4, 50, 51, 500].map((x) => nearestTimeIndex(samples, x))).toEqual([0, 0, 1, 2, 3]);
  });
  it("handles empty, constant and invalid data without a zero-width scale", () => {
    expect(timeSeriesDomain([])).toEqual([0, 1]);
    expect(timeSeriesDomain([5])).toEqual([4, 6]);
    const resolved = timeSeriesDatasets([{ x: NaN, y: 2 }, { x: 4, y: Infinity }, { x: 8, y: 0 }], "Metric", 2);
    expect(resolved[0]!.data).toEqual([{ x: 8, y: 0 }]);
    expect(resolved[0]!.paint).toBe("var(--color-primary)");
    expect(() => timeSeriesDatasets([{ id: "a", label: "A", data: [] }, { id: "a", label: "B", data: [] }], "Metric", 1)).toThrow(/duplicate dataset id/);
    expect(formatTimeSample(new Date("2026-10-16T11:20:00Z"))).toBe("2026-10-16 11:20:00 UTC");
  });
});
