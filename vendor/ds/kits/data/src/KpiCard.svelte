<!-- DS-owned KPI composition; optional chart content is caller-supplied. -->
<script module lang="ts">
  export * from "./core/kpi-card";
</script>

<script lang="ts">
  import {
    type KpiDeltaDirection,
    type KpiPolarity,
    type KpiDeltaSentiment,
    kpiDeltaDirection,
    kpiDeltaSentiment,
    formatKpiDelta,
  } from "./core/kpi-card";
  import type { KpiCardContract } from "./core/contracts/kpicard";
  import type { Snippet } from "svelte";
  import { warnDeprecated } from "@reddb-io/design-system/base";
  import {
    kpiCard,
    type DeprecatedKpiCardTone,
    type KpiCardTone,
  } from "./kpi-card.variants";

  interface Props extends KpiCardContract {
    /** Optional chart content, normally a Sparkline imported by the caller. */
    sparkline?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the card's root `section`. */
    class?: string;
  }

  let {
    label,
    value,
    delta,
    deltaSuffix = "%",
    deltaLabel = "versus previous period",
    description,
    polarity = "higher-is-better",
    tone = "neutral",
    sparkline,
    class: className = "",
  }: Props = $props();

  const direction = $derived(
    delta == null ? undefined : kpiDeltaDirection(delta),
  );
  const sentiment = $derived(
    delta == null ? undefined : kpiDeltaSentiment(delta, polarity),
  );
  const styles = $derived(
    kpiCard({ tone, sentiment, trend: Boolean(sparkline) }),
  );

  $effect(() => {
    if (tone === "brand") {
      warnDeprecated(
        "KpiCard",
        'tone="brand"',
        "a tone (neutral, info, success, warning, danger); the accent is an emphasis, not a tone",
      );
    }
  });
</script>

<section
  data-kpi-card
  data-tone={tone}
  data-delta-direction={direction}
  data-delta-sentiment={sentiment}
  data-polarity={polarity}
  aria-label={label}
  class={styles.root({ class: className })}
>
  <div data-kpi-reading class={styles.reading()}>
    <p data-kpi-label class={styles.label()}>{label}</p>
    <p data-kpi-value class={styles.value()}>{value}</p>
  </div>

  {#if delta != null || description}
    <div data-kpi-details class={styles.details()}>
      {#if delta != null}
        <div data-kpi-context class={styles.context()}>
          {#if delta != null}
            <p data-kpi-delta class={styles.delta()}>
              <span aria-hidden="true"
                >{direction === "increase"
                  ? "↑"
                  : direction === "decrease"
                    ? "↓"
                    : "→"}
              </span><span data-kpi-change
                >{formatKpiDelta(delta, deltaSuffix)}</span
              >
              <span class="sr-only"> {deltaLabel}</span>
            </p>
          {/if}
          <p
            data-kpi-comparison
            aria-hidden="true"
            class={styles.description()}
          >
            {deltaLabel}
          </p>
        </div>
      {/if}

      {#if description}
        <p data-kpi-description class={[styles.description(), "col-span-full"]}>
          {description}
        </p>
      {/if}
    </div>
  {/if}

  {#if sparkline}
    <div data-kpi-sparkline class={styles.sparkline()}>
      {@render sparkline()}
    </div>
  {/if}
</section>
