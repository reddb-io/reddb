<script lang="ts">
  import type { Component } from "svelte";
  import Table from "../../src/Table.svelte";
  import Timeline from "../../src/Timeline.svelte";
  import Steps from "../../src/Steps.svelte";
  import Pagination from "../../src/Pagination.svelte";
  import Countdown from "../../src/Countdown.svelte";
  import ScrollArea from "../../src/ScrollArea.svelte";
  import Breadcrumbs from "../../src/Breadcrumbs.svelte";
  const adapters: Record<string, Component<any>> = { Table, Timeline, Steps, Pagination, Countdown, ScrollArea, Breadcrumbs };
  let { name, options = {}, rich = false }: { name: string; options?: Record<string, any>; rich?: boolean } = $props();
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, unknown>) { options = next; }
</script>
{#if name === "ScrollArea"}
  <ScrollArea {...options} label={options.label}><a href="/detail">Details</a><p>Scrollable content</p></ScrollArea>
{:else if name === "Table" && rich}
  <Table {...options} caption={options.caption} columns={options.columns} rows={options.rows}>
    {#snippet cell({ column, rowIndex, value })}<a href="/cell/{rowIndex}/{column.key}">{value ?? "Missing"}</a>{/snippet}
  </Table>
{:else}
  <Adapter {...options} />
{/if}
