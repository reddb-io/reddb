<!-- Order facts composed into canonical heading and native table relationships. -->
<script lang="ts">
  import type { OrderHistoryContract } from "../core/contracts/orderhistory";
  import {
    SectionHeading,
    Table,
    type SectionHeadingLevel,
    type TableCellContext,
    type TableColumn,
    type TableRow,
  } from "@reddb-io/design-system/base";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    DEFAULT_ORDER_HISTORY_LABELS,
    orderHistory,
    type OrderHistoryEntry,
    type OrderHistoryLabels,
  } from "./order-history.variants";

  interface Props extends OrderHistoryContract, Omit<HTMLAttributes<HTMLElement>, keyof OrderHistoryContract | "class" | "title"> {
    /** Extra classes, merged over the root slot's own, onto the `section` element. */
    class?: string;
  }

  const uid = $props.id();
  const {
    title,
    level = 2,
    caption,
    orders,
    labels = {},
    class: className,
    ...rest
  }: Props = $props();
  const titleId = `${uid}-title`;
  const vocabulary = $derived({ ...DEFAULT_ORDER_HISTORY_LABELS, ...labels });
  const columns = $derived<readonly TableColumn[]>([
    { key: "order", header: vocabulary.order },
    { key: "placed", header: vocabulary.placed },
    { key: "status", header: vocabulary.status },
    { key: "total", header: vocabulary.total },
  ]);
  const rows = $derived<readonly TableRow[]>(
    orders.map((order) => ({
      order: order.id,
      placed: order.placed,
      status: order.status,
      total: order.total,
    })),
  );
  const styles = orderHistory();
</script>

<section
  {...rest}
  data-order-history
  aria-labelledby={rest["aria-label"] ? undefined : titleId}
  class={styles.root({ class: className })}
>
  <SectionHeading id={titleId} {title} {level} rule={false} class={styles.heading()} />
  <Table {caption} {columns} {rows} rowHeader="order" {cell} />
</section>

{#snippet cell({ column, value, rowIndex }: TableCellContext)}
  {#if column.key === "placed"}
    <time datetime={orders[rowIndex]?.placedAt}>{value}</time>
  {:else}
    {value}
  {/if}
{/snippet}
