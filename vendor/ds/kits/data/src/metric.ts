import type { ResolvedTimeSeries } from "./time-series";

/** Statistics over observed samples; missing timestamps never become invented zeroes. */
export function metricStatistics(datasets: readonly ResolvedTimeSeries[]) {
  return datasets.map(dataset => {
    const values = [...dataset.samples.values()];
    const total = values.reduce((sum, value) => sum + value, 0);
    const last = dataset.data.at(-1);
    return {
      id: dataset.id, label: dataset.label, paint: dataset.paint,
      count: values.length,
      min: values.length ? values.reduce((low, value) => Math.min(low, value), Infinity) : null,
      max: values.length ? values.reduce((high, value) => Math.max(high, value), -Infinity) : null,
      average: values.length ? total / values.length : null,
      total: values.length ? total : null,
      last: last?.y ?? null,
    };
  });
}
