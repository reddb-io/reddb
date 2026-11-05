import { TONES, type Tone } from "@reddb-io/design-system/base/recipes";
import { tv, type VariantProps } from "tailwind-variants";

export const kpiCard = tv({
  slots: {
    root: [
      "grid w-full min-w-0 gap-[var(--reddb-spatial-gap-md)] rounded-lg border",
      "p-[var(--reddb-spatial-inset-md)]",
    ].join(" "),
    reading: "flex min-w-0 flex-col justify-between",
    details:
      "col-span-full grid content-start gap-[var(--reddb-spatial-gap-md)]",
    label: "m-0 break-words text-caption",
    // The figure reads in the Theme's title role (ADR 0025): its size, weight,
    // tracking and line height follow the Theme, never a raw step.
    value: "m-0 break-words text-title tabular-nums",
    delta:
      "m-0 inline-flex items-center gap-[var(--reddb-spatial-gap-sm)] text-sm font-medium tabular-nums",
    context:
      "col-span-full flex min-w-0 flex-wrap items-baseline justify-between gap-x-[var(--reddb-spatial-gap-md)] gap-y-[var(--reddb-spatial-gap-sm)]",
    description: "m-0 text-caption",
    sparkline: "min-w-0 [&_[data-chart-kind=sparkline]]:p-0",
  },
  variants: {
    // A requested trend shares the value's band instead of adding another
    // card-sized row. Its fluid figure sizes itself to this own column.
    trend: {
      true: {
        root: "grid-cols-[minmax(0,1fr)_minmax(0,1fr)]",
        sparkline: "col-start-2 row-start-1 self-center",
      },
    },
    tone: {
      neutral: {
        // A neutral card leaves its chart in the Series Role the chart names:
        // a measurement on a quiet ground is never the Brand red.
        root: "border-muted bg-elevation-raised-surface text-foreground",
        label: "text-ink-muted",
        description: "text-ink-muted",
      },
      /**
       * @deprecated The accent is an emphasis, not a tone (ADR 0026): kept one
       * release with its accent fill, then removed. Use a tone, or `neutral`.
       */
      brand: {
        root: "border-[var(--reddb-color-primary)] bg-[var(--reddb-color-primary)] text-[var(--reddb-color-on-primary)] [&_[data-ds-chart]]:[--color-primary:var(--reddb-color-on-primary)]",
      },
      success: {
        root: "border-[var(--reddb-color-feedback-success-border)] bg-[var(--reddb-color-feedback-success-surface)] text-[var(--reddb-color-feedback-success-foreground)] [&_[data-ds-chart]]:[--color-primary:var(--reddb-color-feedback-success-foreground)]",
      },
      warning: {
        root: "border-[var(--reddb-color-feedback-warning-border)] bg-[var(--reddb-color-feedback-warning-surface)] text-[var(--reddb-color-feedback-warning-foreground)] [&_[data-ds-chart]]:[--color-primary:var(--reddb-color-feedback-warning-foreground)]",
      },
      danger: {
        root: "border-[var(--reddb-color-feedback-danger-border)] bg-[var(--reddb-color-feedback-danger-surface)] text-[var(--reddb-color-feedback-danger-foreground)] [&_[data-ds-chart]]:[--color-primary:var(--reddb-color-feedback-danger-foreground)]",
      },
      info: {
        root: "border-[var(--reddb-color-feedback-info-border)] bg-[var(--reddb-color-feedback-info-surface)] text-[var(--reddb-color-feedback-info-foreground)] [&_[data-ds-chart]]:[--color-primary:var(--reddb-color-feedback-info-foreground)]",
      },
    },
    /** The delta's meaning under the card's polarity, never its bare sign. */
    sentiment: {
      improvement: {},
      regression: {},
      neutral: {},
    },
  },
  compoundVariants: [
    {
      tone: "neutral",
      sentiment: "improvement",
      class: {
        delta: "text-feedback-success-foreground",
      },
    },
    {
      tone: "neutral",
      sentiment: "regression",
      class: {
        delta: "text-feedback-danger-foreground",
      },
    },
    {
      tone: "neutral",
      sentiment: "neutral",
      class: { delta: "text-ink-muted" },
    },
  ],
  defaultVariants: { tone: "neutral" },
});

export type KpiCardVariants = VariantProps<typeof kpiCard>;
/** The tones a KpiCard accepts: the shared vocabulary Base exports (ADR 0026). */
export type KpiCardTone = Tone;
/** Tone values kept for one release (ADR 0026): `brand` still draws its accent fill, and warns. */
export const DEPRECATED_KPI_CARD_TONES = ["brand"] as const;
/** @deprecated `brand` is an emphasis, not a tone (ADR 0026). Removed next release. */
export type DeprecatedKpiCardTone = (typeof DEPRECATED_KPI_CARD_TONES)[number];
/** The tones a KpiCard accepts, in the shared order. */
export const KPI_CARD_TONES = TONES satisfies readonly NonNullable<
  KpiCardVariants["tone"]
>[];
