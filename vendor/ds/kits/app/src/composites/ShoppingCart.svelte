<!-- Editable cart rows and an announced total composed inside canonical Form. -->
<script lang="ts">
  import type { ShoppingCartContract } from "../core/contracts/shoppingcart";
  import {
    Button,
    Field,
    Form,
    Input,
    List,
    SectionHeading,
    type SectionHeadingLevel,
  } from "@reddb-io/design-system/base";
  import type { Snippet } from "svelte";
  import type { HTMLFormAttributes } from "svelte/elements";
  import { shoppingCart, type ShoppingCartItem } from "./shopping-cart.variants";

  interface Props extends ShoppingCartContract, Omit<HTMLFormAttributes, keyof ShoppingCartContract | "children" | "class" | "title"> {
    /** Caller-owned cart rows with native media, detail and action snippets. */
    items: readonly ShoppingCartItem[];
    /** Caller-owned recovery content rendered when the cart has no items. */
    empty?: Snippet;
    /** Optional controls, such as checkout, rendered after the total while the cart has items. */
    actions?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the form. */
    class?: string;
    /**
     * Called with the row's `id` and the typed quantity on each input; the quantity is `NaN` when
     * the field is cleared.
     */
    onquantitychange?: (id: string, quantity: number) => void;
    /**
     * Called with the row's `id` when its Remove button is pressed. Without it, no Remove buttons
     * are rendered.
     */
    onremove?: (id: string) => void;
  }

  const uid = $props.id();
  const {
    title,
    level = 2,
    items,
    totalLabel,
    total,
    empty,
    actions,
    class: className,
    itemClass,
    quantityClass,
    onquantitychange,
    onremove,
    ...rest
  }: Props = $props();
  const titleId = `${uid}-title`;
  const itemLevel = $derived(Math.min(level + 1, 6) as SectionHeadingLevel);
  const styles = shoppingCart();
</script>

<Form
  {...rest}
  data-shopping-cart
  aria-labelledby={rest["aria-label"] ? undefined : titleId}
  class={styles.root({ class: className })}
>
  <SectionHeading id={titleId} {title} {level} rule={false} class={styles.heading()} />

  <List gap="lg" class={styles.list()}>
    {#each items as item, index (item.id)}
      <li data-cart-item={item.id} class={styles.item({ class: itemClass })}>
        <div
          class={styles.details({
            class: item.media
              ? "grid-cols-[calc(var(--reddb-spatial-control-height-md)*2)_minmax(0,1fr)]"
              : "grid-cols-1",
          })}
        >
          {#if item.media}
            <div data-cart-item-media class={styles.media()}>{@render item.media()}</div>
          {/if}
          <div class={styles.identity()}>
            <svelte:element this={`h${itemLevel}`} class={styles.name()}>{item.name}</svelte:element>
            {#if item.details}
              {@render item.details()}
            {:else}
              {#if item.description}<p class={styles.description()}>{item.description}</p>{/if}
              <p class={styles.price()}>{item.price}</p>
            {/if}
          </div>
        </div>
        <div class={styles.controls()}>
          <Field label={`Quantity for ${item.name}`} id={`${uid}-${index}-quantity`}>
            {#snippet children(control)}
              <Input
                {...control}
                name={item.quantityName ?? `quantity[${item.id}]`}
                type="number"
                min="0"
                inputmode="numeric"
                value={item.quantity}
                disabled={item.disabled}
                class={styles.quantity({ class: quantityClass })}
                oninput={(event) => onquantitychange?.(item.id, event.currentTarget.valueAsNumber)}
              />
            {/snippet}
          </Field>
          {#if onremove}
            <Button
              type="button"
              variant="secondary"
              disabled={item.disabled}
              onclick={() => onremove(item.id)}
            >Remove {item.name}</Button>
          {/if}
          {#if item.actions}
            <div data-cart-item-actions class={styles.itemActions()}>{@render item.actions()}</div>
          {/if}
        </div>
      </li>
    {:else}
      <li data-cart-empty class={styles.empty()}>
        {#if empty}{@render empty()}{:else}Your cart is empty.{/if}
      </li>
    {/each}
  </List>

  {#if items.length > 0}
    <div data-cart-total class={styles.total()}>
      <span>{totalLabel}</span>
      <output aria-live="polite" aria-atomic="true">{total}</output>
    </div>

    {#if actions}
      <div data-cart-actions class={styles.actions()}>{@render actions()}</div>
    {/if}
  {/if}
</Form>
