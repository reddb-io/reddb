<script module lang="ts">
  import type { PrimaryNavigationItem } from "./navigation.behavior";
  export interface BottomNavigationItem extends PrimaryNavigationItem {
    /** Whether this destination's contextual menu is open, independent from currentId. */
    expanded?: boolean;
    /** Id of the contextual menu controlled by this item. */
    controls?: string;
  }
</script>
<script lang="ts">
  import type { BottomNavigationContract } from "../core/contracts/bottomnavigation";
  import type { HTMLAttributes } from "svelte/elements";
  import { Badge, Icon, Indicator, StatusIndicator } from "@reddb-io/design-system/base";
  import NavItem from "../primitives/NavItem.svelte";
  import { bottomNavigation } from "./bottom-navigation.variants";
  import { navigationBadge, navigationItems, navigationMarkerLabel } from "./navigation.behavior";

  interface Props extends BottomNavigationContract, Omit<HTMLAttributes<HTMLElement>, keyof BottomNavigationContract | "class"> {
    /** Extra classes, merged over the root slot's own, onto the `nav` element. */
    class?: string;
  }

  const { label, items, currentId, class: className, ...rest }: Props = $props();
  const resolvedItems = $derived(navigationItems(items, currentId));
  const styles = $derived(bottomNavigation());
</script>

<nav
  {...rest}
  aria-label={label}
  data-bottom-navigation
  class={styles.root({ class: className })}
>
  <ul class={styles.list()}>
    {#each resolvedItems as item (item.id)}
      <li class={styles.entry()}>
        {#snippet face()}
          {#if item.icon}<Icon icon={item.icon} size="md" color={item.current ? "foreground" : "ink-muted"} aria-hidden="true" />
          {:else if item.fallback}<span aria-hidden="true">{item.fallback}</span>{/if}
        {/snippet}
        {#snippet glyph()}
          <span class={styles.face()}>
            <Indicator class="size-full items-center justify-center">
              {#snippet marker()}{#if item.badge !== undefined}<Badge tone={item.badgeTone ?? "neutral"} variant="filled" class={styles.badge()} aria-hidden="true">{navigationBadge(item.badge)}</Badge>{/if}{/snippet}
              <Indicator position="bottom-end" class="size-full items-center justify-center">
                {#snippet marker()}{#if item.status}<StatusIndicator label={item.status.label} tone={item.status.tone} showLabel={false} aria-hidden="true" />{/if}{/snippet}
                {@render face()}
              </Indicator>
            </Indicator>
          </span>
        {/snippet}
        <NavItem
          label={item.label}
          aria-label={navigationMarkerLabel(item, item.label)}
          aria-expanded={item.expanded}
          aria-controls={item.controls}
          href={item.href}
          current={item.current}
          disabled={item.disabled}
          data-navigation-item={item.id}
          class={styles.item()}
          onclick={item.onselect}
          icon={item.icon || item.fallback || item.badge !== undefined || item.status ? glyph : undefined}
        />
      </li>
    {/each}
  </ul>
</nav>
