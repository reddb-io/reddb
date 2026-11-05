<!--
  ListRow — a Primitive: it imports no other Kit component.

  The element is derived, never declared: a row with an `href` is a link, a row
  with an `onclick` is a button, and a row with neither is a plain <div>. That
  is the whole behavior, and it is what stops the two failure modes a list of
  rows usually has — a <div> with a click handler that no keyboard can reach,
  and a row wearing hover and focus styling that does nothing at all.

  The leading and trailing rails are snippets so a caller can put a Badge, a
  Kbd or an avatar in them without this component importing any of those and
  becoming a Composite.
-->
<script lang="ts">
  import type { ListRowContract } from "../core/contracts/listrow";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { quietControl, warnDeprecated } from "@reddb-io/design-system/base";
  import {
    DEPRECATED_LIST_ROW_DENSITIES,
    listRow,
    type ListRowDensity,
    type ListRowSize,
  } from "./list-row.variants";

  interface Props extends ListRowContract, Omit<HTMLAttributes<HTMLElement>, keyof ListRowContract | "class" | "title"> {
    /** @deprecated Renamed `size` (ADR 0026): `comfortable` is `md`, `compact` is `sm`. Removed next release. */
    density?: ListRowDensity;
    /** Before the text: an icon, an avatar, a status dot. */
    leading?: Snippet;
    /** After the text, pushed to the end of the row. */
    trailing?: Snippet;
    /** Full control over the text block. Replaces `title`/`description`. */
    children?: Snippet;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  const {
    title,
    description,
    href,
    size,
    density,
    selected = false,
    leading,
    trailing,
    children,
    class: className,
    ...rest
  }: Props = $props();

  const tag = $derived(href !== undefined ? "a" : rest.onclick ? "button" : "div");
  const interactive = $derived(tag !== "div");
  // A link needs its destination; a button must not submit the form it happens
  // to stand in, exactly as Button defaults. A <div> needs neither.
  const native = $derived(
    tag === "a" ? { href } : tag === "button" ? { type: "button" as const } : {},
  );

  const resolvedSize = $derived<ListRowSize>(
    size ?? (density !== undefined ? DEPRECATED_LIST_ROW_DENSITIES[density] : "md"),
  );
  const slots = $derived(listRow({ size: resolvedSize, interactive, selected }));

  $effect(() => {
    if (density !== undefined) {
      warnDeprecated("ListRow", `density="${density}"`, `size="${DEPRECATED_LIST_ROW_DENSITIES[density]}"`);
    }
  });
</script>

<svelte:element
  this={tag}
  {...rest}
  {...native}
  class={interactive
    ? quietControl({ ink: "inherit", selected, class: slots.root({ class: className }) })
    : slots.root({ class: className })}
  data-size={resolvedSize}
  aria-current={selected && tag === "a" ? "true" : undefined}
>
  {#if leading}<span class={slots.leading()}>{@render leading()}</span>{/if}

  <span class={slots.text()}>
    {#if children}
      {@render children()}
    {:else}
      {#if title}<span class={slots.title()}>{title}</span>{/if}
      {#if description}<span class={slots.description()}>{description}</span>{/if}
    {/if}
  </span>

  {#if trailing}<span class={slots.trailing()}>{@render trailing()}</span>{/if}
</svelte:element>
