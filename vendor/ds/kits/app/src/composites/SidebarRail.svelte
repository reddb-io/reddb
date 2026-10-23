<!-- A compact, externally selected application/workspace rail with roving focus. -->
<script module lang="ts">
  import { face, nameOf } from "../core/sidebar-rail";
  import type { SidebarRailItem } from "../core/sidebar-rail";
  export type { SidebarRailItem } from "../core/sidebar-rail";
</script>

<script lang="ts">
  import type { SidebarRailContract } from "../core/contracts/sidebarrail";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { Badge, Button, Divider, Icon, Indicator, StatusIndicator, Tooltip } from "@reddb-io/design-system/base";
  import { navigationBadge, navigationMarkerLabel } from "./navigation.behavior";
  import { warnDeprecated } from "@reddb-io/design-system/base";
  import { sidebarRail } from "./sidebar-rail.variants";

  interface Props extends SidebarRailContract, Omit<HTMLAttributes<HTMLElement>, "class" | "onselect"> {
    /** @deprecated Renamed `currentId` (ADR 0026); removed next release. */
    selectedId?: string;
    /** Emits the chosen identity without retaining organization state. */
    onselect?: (id: string) => void;
    /** Brand or home control placed above the roving item collection. */
    top?: Snippet;
    /** Account control placed below the roving item collection. */
    bottom?: Snippet;
    /** Extra classes merged onto the navigation landmark. */
    class?: string;
  }

  let {
    label,
    items,
    currentId: currentIdProp,
    selectedId,
    onselect,
    top,
    bottom,
    showLabels = false,
    class: className,
    ...rest
  }: Props = $props();

  const currentId = $derived(currentIdProp ?? selectedId);

  $effect(() => {
    if (selectedId !== undefined) warnDeprecated("SidebarRail", "selectedId", "currentId");
  });

  function initialFocusIndex(): number {
    const selected = items.findIndex((item) => item.id === currentId && !item.disabled);
    if (selected >= 0) return selected;
    const first = items.findIndex((item) => !item.disabled);
    return Math.max(first, 0);
  }

  let focusIndex = $state(initialFocusIndex());
  const styles = $derived(sidebarRail({ labelled: showLabels }));

  $effect(() => {
    if (items[focusIndex] && !items[focusIndex]?.disabled) return;
    focusIndex = initialFocusIndex();
  });

  function controls(rail: HTMLElement): HTMLElement[] {
    return [...rail.querySelectorAll<HTMLElement>('[data-sidebar-rail-item]:not(:disabled):not([aria-disabled="true"])')];
  }

  function focusControl(event: KeyboardEvent): void {
    if (!(event.target instanceof HTMLElement) || !event.target.dataset.sidebarRailItem) {
      return;
    }

    const available = controls(event.currentTarget as HTMLElement);
    const current = available.indexOf(event.target);
    if (current < 0) return;

    let next: number | undefined;
    if (event.key === "ArrowDown") next = (current + 1) % available.length;
    if (event.key === "ArrowUp") next = (current - 1 + available.length) % available.length;
    if (event.key === "Home") next = 0;
    if (event.key === "End") next = available.length - 1;
    if (next === undefined) return;

    event.preventDefault();
    const control = available[next]!;
    focusIndex = Number(control.dataset.sidebarRailIndex);
    control.focus();
  }

  function rememberFocusedControl(event: FocusEvent): void {
    if (!(event.target instanceof HTMLElement) || !event.target.dataset.sidebarRailItem) {
      return;
    }
    focusIndex = Number(event.target.dataset.sidebarRailIndex);
  }
</script>

<nav
  {...rest}
  aria-label={label}
  data-sidebar-rail
  data-sidebar-rail-labels={showLabels ? "" : undefined}
  class={styles.root({ class: className })}
  onkeydown={focusControl}
  onfocusin={rememberFocusedControl}
>
  <div data-sidebar-rail-region="top" class={styles.region()}>
    {@render top?.()}
  </div>

  <div data-sidebar-rail-region="middle" class={styles.region({ class: styles.middle() })}>
    <ul data-sidebar-rail-items class={styles.list()}>
      {#each items as item, index (item.id)}
        <li class="w-full">
          {#if item.separatorBefore && index > 0}<Divider data-sidebar-rail-divider class={styles.divider()} />{/if}
          {#snippet itemFace()}
            {#if item.icon}<Icon icon={item.icon} size="md" aria-hidden="true" data-sidebar-rail-glyph />
            {:else}<span aria-hidden="true" data-sidebar-rail-glyph class={styles.fallback()}>{face(item)}</span>{/if}
          {/snippet}
          <Tooltip content={navigationMarkerLabel(item, item.label)} disabled={item.disabled}>
            {#snippet child({ props })}
              <Button
                {...props}
                variant="ghost"
                href={item.href}
                aria-label={navigationMarkerLabel(item, nameOf(item, showLabels))}
                disabled={item.disabled}
                tabindex={!item.disabled && index === focusIndex ? 0 : -1}
                aria-pressed={item.href ? undefined : item.id === currentId}
                aria-current={item.href && item.id === currentId ? "page" : undefined}
                aria-expanded={item.expanded}
                aria-controls={item.controls}
                data-sidebar-rail-item={item.id}
                data-sidebar-rail-index={index}
                class={styles.item()}
                onclick={() => onselect?.(item.id)}
              >
            <!-- Read the destination before its decorative face and notification. -->
            {#if showLabels}<span data-sidebar-rail-label class={styles.label()}>{item.label}</span>{/if}
            <span data-sidebar-rail-face class={styles.face()}>
            {#if item.badge !== undefined || item.status}
              <Indicator class={styles.badgeAnchor()}>
                {#snippet marker()}{#if item.badge !== undefined}<Badge tone={item.badgeTone ?? "neutral"} variant="filled" class={styles.badge()} aria-hidden="true">{navigationBadge(item.badge)}</Badge>{/if}{/snippet}
                <Indicator position="bottom-end" class={styles.statusAnchor()}>
                  {#snippet marker()}{#if item.status}<StatusIndicator label={item.status.label} tone={item.status.tone} showLabel={false} aria-hidden="true" />{/if}{/snippet}
                  {@render itemFace()}
                </Indicator>
              </Indicator>
            {:else}{@render itemFace()}{/if}
            </span>
              </Button>
            {/snippet}
          </Tooltip>
        </li>
      {/each}
    </ul>
  </div>

  <div data-sidebar-rail-region="bottom" class={styles.region()}>
    {@render bottom?.()}
  </div>
</nav>
