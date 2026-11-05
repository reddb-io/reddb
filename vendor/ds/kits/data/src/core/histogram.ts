import { formatChartNumber, fractionDigitsFor, type ChartNumberFormat } from "../chart";
export type { ChartNumberFormat, ChartSeries, ChartSize } from "../chart";
export type { ChartLoading } from "../chart-loading";
export interface HistogramBin {
    x0: number;
    x1: number;
    label: string;
    count: number;
}
/**
 * Bin `values` into `requestedBins` equal-width bins. Each bin's label is its
 * edges in `format`, which defaults to just enough decimals to tell
 * neighbouring edges apart (`18–23.7`, never `18–23.666666666666668`).
 */
export function histogramBins(values: readonly number[], requestedBins = 10, format?: ChartNumberFormat): HistogramBin[] {
    const finite = values.filter(Number.isFinite);
    if (finite.length === 0)
        return [];
    let minimum = finite[0]!;
    let maximum = finite[0]!;
    for (const value of finite.slice(1)) {
        minimum = Math.min(minimum, value);
        maximum = Math.max(maximum, value);
    }
    if (minimum === maximum) {
        const edge = format ?? ((value: number) => formatChartNumber(value));
        return [{ x0: minimum, x1: maximum, label: edge(minimum), count: finite.length }];
    }
    const count = Math.max(1, Math.floor(requestedBins));
    const step = (maximum - minimum) / count;
    const digits = fractionDigitsFor(step);
    const edge = format ?? ((value: number) => formatChartNumber(value, digits));
    const bins = Array.from({ length: count }, (_, index) => {
        const x0 = minimum + step * index;
        const x1 = index === count - 1 ? maximum : minimum + step * (index + 1);
        return { x0, x1, label: `${edge(x0)}–${edge(x1)}`, count: 0 };
    });
    for (const value of finite) {
        const index = Math.min(count - 1, Math.floor((value - minimum) / step));
        bins[index]!.count += 1;
    }
    return bins;
}
