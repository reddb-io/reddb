<script lang="ts">
  import KpiCard from "../../src/KpiCard.svelte";
  import Sparkline from "../../src/Sparkline.svelte";
  import type { KpiCardTone } from "../../src/kpi-card.variants";

  let { tone }: { tone?: KpiCardTone } = $props();
</script>

<KpiCard label="Monthly revenue" value="$48k" delta={12.5} {tone}>
  {#snippet sparkline()}
    <Sparkline values={[40, 42, 41, 48]} label="Monthly revenue trend" />
  {/snippet}
</KpiCard>
