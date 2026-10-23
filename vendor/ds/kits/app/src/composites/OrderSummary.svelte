<!-- A named order total composed from canonical Card and DescriptionList. -->
<script lang="ts">
  import type { OrderSummaryContract } from "../core/contracts/ordersummary";
  import {
    Card,
    DescriptionList,
    SectionHeading,
    type SectionHeadingLevel,
  } from "@reddb-io/design-system/base";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { orderSummary, type OrderSummaryLine } from "./order-summary.variants";

  interface Props extends OrderSummaryContract, Omit<HTMLAttributes<HTMLDivElement>, keyof OrderSummaryContract | "class" | "title"> {
    /** Optional controls rendered in the footer, such as a checkout link or button. */
    actions?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the underlying Card. */
    class?: string;
  }

  const uid = $props.id();
  const {
    title,
    level = 2,
    lines,
    totalLabel,
    total,
    actions,
    class: className,
    ...rest
  }: Props = $props();
  const titleId = `${uid}-title`;
  const styles = orderSummary();
</script>

<Card
  {...rest}
  data-order-summary
  role="region"
  aria-labelledby={rest["aria-label"] ? undefined : titleId}
  class={styles.root({ class: className })}
>
  {#snippet header()}
    <SectionHeading id={titleId} {title} {level} rule={false} class={styles.heading()} />
  {/snippet}

  <div class={styles.content()}>
    <DescriptionList items={lines.map((line) => ({ term: line.label, detail: line.value }))} />
    <div data-order-summary-total class={styles.total()}>
      <span>{totalLabel}</span>
      <output aria-live="polite" aria-atomic="true">{total}</output>
    </div>
  </div>

  {#snippet footer()}
    {#if actions}
      <div data-order-summary-actions class={styles.actions()}>{@render actions()}</div>
    {/if}
  {/snippet}
</Card>
