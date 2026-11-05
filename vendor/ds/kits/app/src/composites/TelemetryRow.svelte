<script lang="ts">
  import type { Snippet } from "svelte";
  import { Meter, StatusIndicator } from "@reddb-io/design-system/base";
  import type { TelemetryRowContract } from "../core/contracts/telemetryrow";
  import { operational } from "../operational.variants";

  interface Props extends TelemetryRowContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    value,
    max = 100,
    stateLabel,
    condition,
    description,
    tone = "neutral",
    class: className,
  }: Props = $props();
  const s = operational();
</script>

<div data-telemetry-row class={s.body({ class: className })}>
  <div class={s.header()}>
    <p class={s.title()}>{label}</p>
    {#if stateLabel}<StatusIndicator label={stateLabel} {tone} />{/if}
  </div>
  <Meter
    {value}
    {max}
    {label}
    layout="inline"
    labelHidden
    formatValue={(n) => `${n} / ${Math.max(1, max)}`}
  />{#if description || condition}<div class={s.row()}>
      {#if description}<span class={s.description()}>{description}</span
        >{/if}{#if condition}<span class={s.description()}>{condition}</span
        >{/if}
    </div>{/if}
</div>
