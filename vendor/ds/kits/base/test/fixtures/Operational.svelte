<script lang="ts">
  import { Progress, Meter, Statistic, Transfer } from "../../src";
  let { name, options }: { name: string; options: Record<string, any> } =
    $props();
</script>

{#if name === "Progress"}<Progress
    {...options}
  />{:else if name === "Meter"}<Meter
    value={options.value}
    {...options}
  />{:else if name === "Statistic"}<Statistic
    label={options.label}
    value={options.value}
    {...options}
  />{:else}<Transfer
    fromId={options.fromId}
    toId={options.toId}
    {...options}
  />{/if}
