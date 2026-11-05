<script lang="ts">
  import { Table } from "@reddb-io/design-system/base";

  const columns = [
    { key: "node", header: "Node" },
    { key: "region", header: "Region" },
    { key: "latency", header: "Latency" },
  ] as const;
  const rows = [
    { node: "db-01", region: "South America", latency: "24 ms" },
    { node: "db-02", region: "Europe", latency: "31 ms" },
  ] as const;
</script>

{#snippet cell({ column, value })}
  {#if column.key === "node"}
    <a href={`/nodes/${value}`}>{value}</a>
  {:else}
    {value}
  {/if}
{/snippet}

<section data-table-scope data-theme="marketing" data-color-scheme="dark" data-density="compact">
  <Table caption="Cluster replicas" {columns} {rows} rowHeader="node" {cell} />
</section>
