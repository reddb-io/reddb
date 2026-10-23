<script lang="ts">
  import type { Snippet } from "svelte";
  import { StatusIndicator } from "@reddb-io/design-system/base";
  import type { PhaseSummaryContract } from "../core/contracts/phasesummary";
  import { operational } from "../operational.variants";
  import { validateGates, GATE_LABELS, GATE_TONES } from "../core/operational";
  interface Props extends PhaseSummaryContract {
    /** Additional root classes. */
    class?: string;
  }
  let { label, gates, description, class: className }: Props = $props();
  const s = operational();
</script>

<section
  data-phase-summary
  aria-label={label}
  class={s.body({ class: className })}
>
  <p class={s.title()}>{label}</p>
  {#if description}<p class={s.description()}>{description}</p>{/if}
  <ul class={s.list()}>
    {#each validateGates(gates) as gate (gate.id)}<li
        data-gate-id={gate.id}
        data-state={gate.state}
      >
        <div class={s.row()}>
          <span class={s.title()}>{gate.label}</span><StatusIndicator
            label={GATE_LABELS[gate.state]}
            tone={GATE_TONES[gate.state]}
          />
        </div>
        {#if gate.description}<p class={s.description()}>
            {gate.description}
          </p>{/if}
      </li>{/each}
  </ul>
</section>
