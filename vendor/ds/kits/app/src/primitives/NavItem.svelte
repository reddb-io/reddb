<!--
  NavItem — a Primitive: it imports no other Kit component.

  One entry in a navigation list. Like ListRow it derives its element — an <a>
  when it goes somewhere, a <button> when it only does something — but unlike
  ListRow it also has to say *where you are*: `current` sets `aria-current`, so
  the current page is announced and not merely tinted. Colour alone would leave
  a screen reader with a list of identical links.

  The trailing rail is a snippet, which is how a caller hangs a Badge or a Kbd
  off an item without this component importing either.
-->
<script lang="ts">
  import type { NavItemContract } from "../core/contracts/navitem";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { quietControl, warnDeprecated } from "@reddb-io/design-system/base";
  import { navItem } from "./nav-item.variants";

  /** What `aria-current` says: `page` for a route, `step` in a wizard, `location` in a trail. */
  type CurrentToken = "page" | "step" | "location" | "date" | "time" | "true";

  interface Props extends NavItemContract, Omit<HTMLAttributes<HTMLElement>, keyof NavItemContract | "class"> {
    /** @deprecated Renamed `current` (ADR 0026): `active` is `current`, `active current="step"` is `current="step"`. Removed next release. */
    active?: boolean;
    /** Before the label. */
    icon?: Snippet;
    /** After the label, pushed to the end of the item. */
    trailing?: Snippet;
    /** Full control over the label. */
    children?: Snippet;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  const {
    label,
    href,
    current = false,
    active,
    disabled = false,
    icon,
    trailing,
    children,
    class: className,
    ...rest
  }: Props = $props();

  // A caller still on the deprecated `active` keeps its old meaning for one
  // release: `active` decides current-ness, and a token in `current` only names it.
  const isCurrent = $derived(active !== undefined ? active : current !== false);
  const token = $derived<CurrentToken>(typeof current === "string" ? current : "page");

  $effect(() => {
    if (active !== undefined) warnDeprecated("NavItem", "active", "current");
  });

  const tag = $derived(href !== undefined ? "a" : "button");
  // `pointer-events-none` stops the pointer; only these keep a keyboard out. A
  // disabled link is not a thing the platform has, so its href is withheld —
  // which is exactly what makes it unfocusable.
  const native = $derived(
    tag === "a"
      ? { href: disabled ? undefined : href, tabindex: disabled ? -1 : undefined }
      : { type: "button" as const, disabled },
  );
</script>

<svelte:element
  this={tag}
  {...rest}
  {...native}
  data-nav-item
  class={quietControl({ selected: isCurrent, class: navItem({ current: isCurrent, disabled }).root({ class: className }) })}
  aria-current={isCurrent ? token : undefined}
  aria-disabled={disabled ? "true" : undefined}
>
  <span class={navItem().label()}>
    {#if children}{@render children()}{:else}{label ?? ""}{/if}
  </span>

  {#if icon}
    <span class={navItem().icon()} aria-hidden="true">{@render icon()}</span>
  {/if}

  {#if trailing}<span class={navItem().trailing()}>{@render trailing()}</span>{/if}
</svelte:element>
