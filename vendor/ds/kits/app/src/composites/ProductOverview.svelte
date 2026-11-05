<!-- Product identity and caller-owned purchase content in one reusable overview. -->
<script lang="ts">
  import type { ProductOverviewContract } from "../core/contracts/productoverview";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    Image,
    SectionHeading,
    Stack,
    type ImageAsset,
    type SectionHeadingLevel,
  } from "@reddb-io/design-system/base";
  import { productOverview } from "./product-overview.variants";

  interface Props extends ProductOverviewContract, Omit<HTMLAttributes<HTMLElement>, keyof ProductOverviewContract | "children" | "class" | "title"> {
    /**
     * Caller-owned product imagery, including its own alternative text. Optional when `image` is
     * given; one of the two is required.
     */
    media?: Snippet;
    /** Caller-owned product detail, options, or fulfilment information. */
    children?: Snippet;
    /** Caller-owned canonical purchase controls. */
    actions?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the `section` element. */
    class?: string;
  }

  const instanceId = $props.id();
  const headingId = `${instanceId}-title`;
  const {
    title,
    description,
    price,
    level = 2,
    media,
    image,
    priority = false,
    children,
    actions,
    class: className,
    ...rest
  }: Props = $props();
  const slots = $derived(productOverview());
</script>

<section
  {...(rest as Record<string, unknown>)}
  data-product-overview
  aria-labelledby={headingId}
  class={slots.root({ class: className })}
>
  <div data-product-overview-media class={slots.media()}>
    {#if media}
      {@render media()}
    {:else if image}
      <Image {...image} {priority} class="w-full" />
    {/if}
  </div>
  <Stack gap="layout-sm" class={slots.content()}>
    <SectionHeading
      {title}
      {description}
      {level}
      titleId={headingId}
      rule={false}
      class={slots.heading()}
    />
    {#if price}<p data-product-overview-price class={slots.price()}>{price}</p>{/if}
    {#if children}
      <div data-product-overview-body class={slots.body()}>{@render children()}</div>
    {/if}
    {#if actions}
      <div data-product-overview-actions class={slots.actions()}>{@render actions()}</div>
    {/if}
  </Stack>
</section>
