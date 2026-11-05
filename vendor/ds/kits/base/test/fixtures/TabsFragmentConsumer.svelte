<script lang="ts">
  import { Tabs } from "@reddb-io/design-system/base";

  const items = [
    { value: "table", label: "Table" },
    { value: "graph", label: "Graph" },
  ] as const;
</script>

<Tabs label="Result format" {items} followHash>
  {#snippet children({ value })}
    {#if value === "table"}
      <div id="source-table" tabindex="-1">Table evidence</div>
    {:else}
      <div id="source-graph" tabindex="-1">Graph evidence</div>
    {/if}
  {/snippet}
</Tabs>
