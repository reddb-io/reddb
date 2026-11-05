<script lang="ts">
  import type { SidebarNavigationContract } from "../core/contracts/sidebarnavigation";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { Badge, Icon, Indicator, StatusIndicator } from "@reddb-io/design-system/base";
  import NavItem from "../primitives/NavItem.svelte";
  import { navigationBadge, navigationItems, navigationMarkerLabel, type ApplicationNavigationItem } from "./navigation.behavior";
  import { sidebarNavigation } from "./sidebar-navigation.variants";

  interface Props extends SidebarNavigationContract, Omit<HTMLAttributes<HTMLElement>, "class"> {
    /** Optional contextual actions above the destinations, such as Back, a workspace selector or Close. Native controls retain their normal tab order. */
    toolbar?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the `nav` element. */
    class?: string;
  }

  const { label, items, currentId, toolbar, toolbarLabel = "Sidebar actions", class: className, ...rest }: Props = $props();
  const resolvedItems = $derived(navigationItems(items, currentId));
  const styles = $derived(sidebarNavigation());
</script>

<nav
  {...rest}
  aria-label={label}
  data-sidebar-navigation
  class={styles.root({ class: className })}
>
  {#if toolbar}
    <div role="group" aria-label={toolbarLabel} data-sidebar-toolbar class={styles.toolbar()}>
      {@render toolbar()}
    </div>
  {/if}
  <ul class={styles.list()}>
    {#each resolvedItems as item (item.id)}
      <li>
        {#snippet glyph()}
          {#if item.icon}
            {@const destinationGlyph = item.icon}
            {#snippet destinationIcon()}
            <Icon
              icon={destinationGlyph}
              size="sm"
              color={item.current ? "foreground" : "ink-muted"}
              aria-hidden="true"
            />
            {/snippet}
            {#if item.status}
              <Indicator position="bottom-end">
                {#snippet marker()}{#if item.status}<StatusIndicator label={item.status.label} tone={item.status.tone} showLabel={false} />{/if}{/snippet}
                {@render destinationIcon()}
              </Indicator>
            {:else}{@render destinationIcon()}{/if}
          {/if}
        {/snippet}
        {#snippet markers()}
          {#if item.status && !item.icon}<StatusIndicator label={item.status.label} tone={item.status.tone} showLabel={false} aria-hidden="true" />{/if}
          {#if item.badge !== undefined}<Badge tone={item.badgeTone ?? "neutral"} variant="filled" aria-hidden="true">{navigationBadge(item.badge)}</Badge>{/if}
        {/snippet}
        <NavItem
          label={item.label}
          aria-label={navigationMarkerLabel(item, item.label)}
          href={item.href}
          current={item.current}
          disabled={item.disabled}
          data-navigation-item={item.id}
          class={styles.item()}
          onclick={item.onselect}
          icon={item.icon ? glyph : undefined}
          trailing={item.badge !== undefined || item.status && !item.icon ? markers : undefined}
        />
      </li>
    {/each}
  </ul>
</nav>
