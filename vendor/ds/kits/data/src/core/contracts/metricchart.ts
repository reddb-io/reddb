import type { IconGlyph } from "@reddb-io/design-system/base";
import type { TimeSeriesDataset, TimeSeriesVariant } from "../../time-series";
import type { ChartCurve, ChartNumberFormat, ChartSize } from "../../chart";
/** Portable MetricChart properties shared by native adapters. */
export interface MetricChartContract {
    /** Visible metric title and accessible name shared by the summary and detail. */
    label: string;
    /** Named measurements; ids and explicit Series Roles preserve identity between views. */
    data: readonly TimeSeriesDataset[];
    /** Visible period or explanation below the title. */
    description?: string;
    /** Headline aggregation across observed samples: `total` sums counters, `average` is sample-weighted, and `last` sums the last observed value of each series. Visibility never changes the headline. */
    aggregate?: "total" | "average" | "last";
    /** Formats every headline, legend, axis and table value. */
    format?: ChartNumberFormat;
    /** Formats time-axis ticks and the shared detailed reading. */
    formatX?: (value: number | Date) => string;
    /** Stacked columns by default; line and area use the same summary/detail contract. */
    variant?: TimeSeriesVariant;
    /** Smooth monotone curves by default; `linear` preserves straight segments in both the summary and detail. Bars are unchanged. */
    curve?: ChartCurve;
    /** `summary` omits axes and grid; `full` includes axes and sample exploration. */
    presentation?: "summary" | "full";
    /** Shared Density-relative plot height. Defaults to `sm` for summaries and `md` for full charts. */
    size?: ChartSize;
    /** Explicit plot height in pixels, shared with other charts when supplied. */
    height?: number;
    /** Optional application-owned detail destination, rendered as a native Explore link. */
    detailHref?: string;
    /** Visible text of the application-owned detail link. */
    detailLabel?: string;
    /** Whether to offer the built-in expanded view. Defaults to true. */
    expandable?: boolean;
    /** Optional consumer-supplied glyph for the expanded-view action. Omitted, its visible text is Expand. Always rendered through Base Icon. */
    expandIcon?: IconGlyph;
    /** Bindable visibility of the expanded view. */
    expanded?: boolean;
    /** Dataset ids hidden in both summary and expanded plots. Bindable. */
    hiddenSeries?: readonly string[];
}
