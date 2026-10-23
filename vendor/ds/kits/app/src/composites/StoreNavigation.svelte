<script lang="ts">
  import type { StoreNavigationContract } from "../core/contracts/storenavigation";
  import { Navbar, type NavbarCollapse, type NavbarLink } from "@reddb-io/design-system/base";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { navigationItems, type ApplicationNavigationItem } from "./navigation.behavior";
  import { storeNavigation } from "./store-navigation.variants";

  interface Props extends StoreNavigationContract, Omit<HTMLAttributes<HTMLElement>, keyof StoreNavigationContract | "class"> {
    /** Optional banner rendered above the navbar, such as a promotion or shipping notice. */
    announcement?: Snippet;
    /** Optional content at the start of the bar, usually the store's logo or name. */
    brand?: Snippet;
    /** Optional controls at the end of the bar, such as search, account or cart buttons. */
    actions?: Snippet;
    /**
     * Extra classes, merged over the root slot's own, onto the wrapper around the announcement and
     * navbar.
     */
    class?: string;
  }

  const {
    label,
    items,
    currentId,
    collapse = "responsive",
    announcement,
    brand,
    actions,
    class: className,
    ...rest
  }: Props = $props();

  const links: readonly NavbarLink[] = $derived(
    navigationItems(items, currentId).map((item) => ({
      id: item.id,
      label: item.label,
      href: item.href,
      current: item.current,
      disabled: item.disabled,
      onselect: item.onselect,
    })),
  );
  const styles = $derived(storeNavigation());
</script>

<div {...rest} data-store-navigation="" class={styles.root({ class: className })}>
  {#if announcement}
    <div data-store-announcement="" class={styles.announcement()}>{@render announcement()}</div>
  {/if}
  <Navbar {label} {links} {collapse} {brand} {actions} class={styles.navbar()} />
</div>
