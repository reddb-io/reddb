<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import { formatPercent, isDeterminate, normalizeRange } from "./progress.behavior";
  import {
    radialProgress,
    type RadialProgressSize,
  } from "./radial-progress.variants";
  import type { Tone } from "./tone";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** Omit the value for an indeterminate radial indicator. */
    value?: number | null;
    /** Lower bound of the scale, exposed as `aria-valuemin`; defaults to `0`. */
    min?: number;
    /**
     * Upper bound of the scale, exposed as `aria-valuemax`; defaults to `100`. A `max` not above
     * `min` is widened to one unit.
     */
    max?: number;
    /** Accessible name of the progress indicator; defaults to `Progress`. */
    label?: string;
    /**
     * Diameter of the ring, following the Density control heights: `sm`, `md` or `lg`. Defaults
     * to `md`.
     */
    size?: RadialProgressSize;
    /**
     * Formats the clamped value for the centered text and `aria-valuetext`; defaults to a rounded
     * percentage.
     */
    formatValue?: (value: number) => string;
    /** What the progress means: the shared tone vocabulary (ADR 0026). Defaults to `neutral` (ink). */
    tone?: Tone;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  const {
    value,
    min = 0,
    max = 100,
    label = "Progress",
    size = "md",
    formatValue,
    tone = "neutral",
    class: className,
    ...rest
  }: Props = $props();

  const determinate = $derived(isDeterminate(value));
  const range = $derived(normalizeRange(isDeterminate(value) ? value : min, min, max));
  const valueText = $derived(
    determinate ? (formatValue?.(range.value) ?? formatPercent(range)) : "In progress",
  );
  const slots = $derived(radialProgress({ size, tone, indeterminate: !determinate }));
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-radial-progress
  data-state={determinate ? "determinate" : "indeterminate"}
  class={slots.root({ class: className })}
  role="progressbar"
  aria-label={label}
  aria-valuemin={range.min}
  aria-valuemax={range.max}
  aria-valuenow={determinate ? range.value : undefined}
  aria-valuetext={valueText}
>
  <svg class={slots.svg()} viewBox="0 0 36 36" aria-hidden="true">
    <circle class={slots.track()} cx="18" cy="18" r="15.5" stroke-width="3" />
    <circle
      class={slots.indicator()}
      cx="18"
      cy="18"
      r="15.5"
      pathLength="100"
      stroke-width="3"
      stroke-linecap="round"
      stroke-dasharray={determinate ? 100 : 25}
      stroke-dashoffset={determinate ? 100 - range.percent : 0}
    />
  </svg>
  <span class={slots.value()}>{valueText}</span>
</div>
