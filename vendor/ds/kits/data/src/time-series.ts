import type { ChartSeries } from "./chart";

export interface TimeSeriesPoint {
  x: number | Date;
  y: number;
}

/** One named measurement on a shared time and value scale. */
export interface TimeSeriesDataset {
  /** Stable identity: visibility follows this id when data is refreshed or reordered. */
  id: string;
  /** Visible legend name. */
  label: string;
  /** Points in this measurement; missing timestamps are not interpolated in the readout. */
  data: readonly TimeSeriesPoint[];
  /** DS Series Role. Defaults to the dataset's position, cycling through the six roles. */
  series?: ChartSeries;
}

export interface ResolvedTimeSeries extends TimeSeriesDataset {
  paint: string;
  samples: ReadonlyMap<number, number>;
}

export function timeSeriesDatasets(
  data: readonly TimeSeriesPoint[] | readonly TimeSeriesDataset[],
  label: string,
  series: ChartSeries,
): ResolvedTimeSeries[] {
  const multiple = data.length > 0 && "data" in data[0]!;
  const datasets: readonly TimeSeriesDataset[] = multiple
    ? data as readonly TimeSeriesDataset[]
    : [{ id: "series", label, data: data as readonly TimeSeriesPoint[], series }];
  const ids = new Set<string>();
  return datasets.map((dataset, index) => {
    if (ids.has(dataset.id)) throw new Error(`TimeSeries: duplicate dataset id "${dataset.id}"`);
    ids.add(dataset.id);
    const role = dataset.series ?? ((index % 6) + 1) as ChartSeries;
    return {
      ...dataset,
      samples: new Map(dataset.data.filter((point) => Number.isFinite(+point.x) && Number.isFinite(point.y)).map((point) => [+point.x, point.y])),
      // Single-series charts retain the inherited paint bridge (e.g. KpiCard).
      paint: multiple ? `var(--reddb-color-series-${role})` : "var(--color-primary)",
      data: dataset.data.filter((point) => Number.isFinite(+point.x) && Number.isFinite(point.y))
        .sort((a, b) => +a.x - +b.x),
    };
  });
}

export function timeSeriesSamples(datasets: readonly TimeSeriesDataset[]): (number | Date)[] {
  return [...new Map(datasets.flatMap((dataset) => dataset.data.map((point) => [+point.x, point.x] as const))).values()]
    .sort((a, b) => +a - +b);
}

export function nearestTimeIndex(samples: readonly (number | Date)[], value: number): number {
  let low = 0;
  let high = samples.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (+samples[middle]! < value) low = middle + 1;
    else high = middle;
  }
  if (low === 0) return 0;
  if (low === samples.length) return Math.max(0, low - 1);
  return value - +samples[low - 1]! <= +samples[low]! - value ? low - 1 : low;
}

export function timeSeriesDomain(samples: readonly (number | Date)[]): [number, number] {
  const first = +(samples[0] ?? 0);
  const last = +(samples.at(-1) ?? 1);
  return first === last ? [first - 1, last + 1] : [first, last];
}

export function formatTimeSample(value: number | Date): string {
  return value instanceof Date ? `${value.toISOString().slice(0, 19).replace("T", " ")} UTC` : String(value);
}

/** Date axes label observed timestamps; numeric coordinates keep the scale's native ticks. */
export function timeSeriesDateTicks(samples: readonly (number | Date)[], capacity: number): number[] | undefined {
  if (!samples.some(sample => sample instanceof Date)) return undefined;
  const count = Math.min(samples.length, Math.max(2, Math.min(12, Math.floor(capacity))));
  return Array.from({ length: count }, (_, index) => +samples[Math.round(index * (samples.length - 1) / Math.max(1, count - 1))]!);
}

/** Compact UTC labels follow the observed time span; the readout retains its exact timestamp. */
export function formatTimeTick(value: number | Date, samples: readonly (number | Date)[]): string {
  const stamp = new Date(+value).toISOString();
  const span = +(samples.at(-1) ?? value) - +(samples[0] ?? value);
  const step = samples.slice(1).reduce<number>((smallest, sample, index) => Math.min(smallest, +sample - +samples[index]!), Infinity);
  return span < 1_000 ? stamp.slice(11, 23) : span < 60_000 ? stamp.slice(11, 19) : span < 86_400_000 ? stamp.slice(11, 16)
    : step < 86_400_000 ? stamp.slice(5, 16).replace("T", " ")
    : span < 366 * 86_400_000 ? stamp.slice(5, 10) : stamp.slice(0, 10);
}

/** Explicit value bounds stay exact; an area must retain its zero baseline. */
export function timeSeriesValueDomain(domain: readonly [number, number] | undefined, area: boolean): [number, number] | undefined {
  if (domain === undefined) return undefined;
  const [low, high] = domain;
  if (!Number.isFinite(low) || !Number.isFinite(high) || low >= high) {
    throw new Error("TimeSeries: valueDomain must have two finite, increasing bounds");
  }
  if (area && (low > 0 || high < 0)) throw new Error("TimeSeries: an area valueDomain must include zero");
  return [low, high];
}

/** Temporal columns share sample positions and diverge around zero. Missing samples remain absent. */
export function timeSeriesBars(datasets: readonly ResolvedTimeSeries[], timestamps: readonly (number | Date)[] = timeSeriesSamples(datasets)) {
  const samples = timestamps.map(Number);
  const positive = new Map<number, number>();
  const negative = new Map<number, number>();
  return datasets.map(dataset => ({
    id: dataset.id,
    paint: dataset.paint,
    data: samples.flatMap((x, index) => {
      const value = dataset.samples.get(x);
      if (value === undefined) return [];
      const sums = value < 0 ? negative : positive;
      const y = sums.get(x) ?? 0;
      sums.set(x, y + value);
      const previous = samples[index - 1];
      const next = samples[index + 1];
      const step = Math.min(previous === undefined ? Infinity : x - previous, next === undefined ? Infinity : next - x);
      const half = (Number.isFinite(step) ? step : 1) * 0.36;
      return [{ x: x - half, x1: x + half, y, y1: y + value }];
    }),
  }));
}

/** Stacked areas retain missing-observation gaps instead of inventing zero contributions. */
export function timeSeriesAreas(datasets: readonly ResolvedTimeSeries[], timestamps: readonly (number | Date)[]) {
  const positive = new Map<number, number>();
  const negative = new Map<number, number>();
  const complete = new Set(timestamps.map(Number).filter(x => datasets.every(dataset => dataset.samples.has(x))));
  return datasets.map(dataset => ({
    id: dataset.id,
    data: timestamps.map(stamp => {
      const x = +stamp;
      const value = dataset.samples.get(x);
      if (value === undefined || !complete.has(x)) return { x, y: 0, y1: 0, defined: false };
      const sums = value < 0 ? negative : positive;
      const y = sums.get(x) ?? 0;
      sums.set(x, y + value);
      return { x, y, y1: y + value, defined: true };
    }),
  }));
}

/** Shared temporal mark vocabulary. */
export type TimeSeriesVariant = "line" | "area" | "stacked-area" | "bar";
