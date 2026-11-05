<!-- Product detail inside the canonical Dialog focus and dismissal lifecycle. -->
<script lang="ts">
  import type { ProductQuickviewContract } from "../core/contracts/productquickview";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { Dialog } from "@reddb-io/design-system/base";
  import { productQuickview } from "./product-quickview.variants";

  interface Props extends ProductQuickviewContract, Omit<HTMLAttributes<HTMLDivElement>, keyof ProductQuickviewContract | "children" | "class" | "title"> {
    /**
     * Custom content for the opening Button; `triggerLabel` stays its accessible name, so words it
     * shows must appear, in order, in `triggerLabel`, ideally first (WCAG 2.5.3, Label in Name):
     * `Quick view` shown, `Quick view Field notes` named.
     */
    trigger?: Snippet;
    /** Caller-owned product imagery, including its own alternative text. */
    media?: Snippet;
    /** Caller-owned product detail and controls. */
    children?: Snippet;
    /** Controls for the dialog's action region; it receives `{ close }` to dismiss the dialog. */
    actions?: Snippet<[{ close: () => void }]>;
    /** Extra classes, merged over the root slot's own, onto the wrapper around the dialog. */
    class?: string;
  }

  const {
    triggerLabel,
    title,
    description,
    trigger,
    media,
    children,
    actions,
    class: className,
    dialogClass,
    ...rest
  }: Props = $props();
  const slots = $derived(productQuickview());
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-product-quickview
  class={slots.root({ class: className })}
>
  <Dialog
    {triggerLabel}
    {title}
    {description}
    {trigger}
    {actions}
    initialFocus="body"
    class={slots.dialog({ class: dialogClass })}
  >
    <div data-product-quickview-content class={slots.content()}>
      {#if media}
        <div data-product-quickview-media class={slots.media()}>{@render media()}</div>
      {/if}
      <div data-product-quickview-body class={slots.body()}>{@render children?.()}</div>
    </div>
  </Dialog>
</div>
