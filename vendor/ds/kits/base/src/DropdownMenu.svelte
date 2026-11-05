<script lang="ts">
  import { portalAppearance } from "./portal-appearance.svelte";
  import type { Snippet } from "svelte";
  import { DropdownMenu as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import Icon from "./Icon.svelte";
  import Kbd from "./Kbd.svelte";
  import type { ButtonVariant } from "./button.variants";
  import {
    dropdownMenuGroups,
    dropdownMenuItems,
    type DropdownMenuEntry,
    type DropdownMenuItem,
  } from "./dropdown-menu.behavior";
  import { dropdownMenu, type DropdownMenuSize } from "./dropdown-menu.variants";
  import { popover as popoverAppearance } from "./popover.variants";

  type Side = "top" | "right" | "bottom" | "left";
  type Align = "start" | "center" | "end";

  interface Props {
    /**
     * Accessible name for the Button that opens the menu, and its visible text when no `trigger` is
     * given.
     */
    triggerLabel: string;
    /** Accessible name for the opened menu surface. */
    contentLabel: string;
    /**
     * The menu entries, in order: single items, or groups with an optional heading. A separator
     * divides groups.
     */
    items?: readonly DropdownMenuEntry[];
    /** Whether the menu is open, bindable. Defaults to closed. */
    open?: boolean;
    /** Called with the next open state (ADR 0026's open dialect). */
    onopenchange?: (open: boolean) => void;
    /** Button emphasis for the trigger. Defaults to `secondary`. */
    variant?: ButtonVariant;
    /** Size step for the trigger Button and the menu rows: `sm`, `md` (the default) or `lg`. */
    size?: DropdownMenuSize;
    /**
     * Preferred side of the trigger for the menu; collision handling may flip it. Defaults to
     * `bottom`.
     */
    side?: Side;
    /** Preferred alignment of the menu along the selected side. Defaults to `start`. */
    align?: Align;
    /** Distance in pixels between the trigger and the menu. Defaults to `8`. */
    sideOffset?: number;
    /** Safe distance in pixels kept between the menu and the viewport edges. Defaults to `8`. */
    collisionPadding?: number;
    /**
     * Custom content inside the trigger Button, replacing the `triggerLabel` text; `triggerLabel`
     * stays its accessible name, so words it shows must appear, in order, in `triggerLabel`,
     * ideally first (WCAG 2.5.3, Label in Name).
     */
    trigger?: Snippet;
    /** Extra classes merged onto the menu surface, not the trigger Button. */
    class?: string;
  }

  let {
    triggerLabel,
    contentLabel,
    items = [],
    open = $bindable(false),
    onopenchange,
    variant = "secondary",
    size = "md",
    side = "bottom",
    align = "start",
    sideOffset = 8,
    collisionPadding = 8,
    trigger,
    class: className,
  }: Props = $props();

  const uid = $props.id();
  const contentId = `${uid}-menu`;
  const groups = $derived(dropdownMenuGroups(items));
  const hasIcons = $derived(dropdownMenuItems(items).some((item) => item.icon !== undefined));
  const slots = $derived(dropdownMenu({ size }));

  function choose(item: DropdownMenuItem): void {
    item.onselect?.();
  }
  const portal = portalAppearance(() => open);
</script>

{#snippet row(item: DropdownMenuItem)}
  {#if hasIcons}
    <span data-menu-item-icon aria-hidden="true" class={slots.icon()}>
      {#if item.icon !== undefined}<Icon icon={item.icon} {size} aria-hidden="true" />{/if}
    </span>
  {/if}
  <span class={slots.label()}>{item.label}</span>
  {#if item.shortcut !== undefined}
    <span class={slots.shortcut()}><Kbd keys={item.shortcut} size="sm" /></span>
  {/if}
{/snippet}

<Bits.Root bind:open onOpenChange={onopenchange}>
  <Bits.Trigger>
    {#snippet child({ props })}
      <Button
        {...props}
        {...portal.anchor}
        data-dropdown-menu-trigger
        aria-label={triggerLabel}
        {variant}
        {size}
      >
        {#if trigger}{@render trigger()}{:else}{triggerLabel}{/if}
      </Button>
    {/snippet}
  </Bits.Trigger>

  <Bits.Portal to={portal.container}>
    <Bits.Content
      id={contentId}
      {side}
      {align}
      {sideOffset}
      avoidCollisions={true}
      {collisionPadding}
      loop={true}
    >
      {#snippet child({ props, wrapperProps })}
        <div {...wrapperProps} {...portal.attributes}>
          <div
            {...props}
            id={contentId}
            data-dropdown-menu-surface
            data-collision-padding={collisionPadding}
            aria-label={contentLabel}
            class={popoverAppearance({ class: slots.content({ class: className }) })}
          >
            {#each groups as group, index (index)}
              {#if index > 0}
                <Bits.Separator>
                  {#snippet child({ props: separatorProps })}
                    <div
                      {...separatorProps}
                      role="separator"
                      aria-orientation="horizontal"
                      class={slots.separator()}
                      data-dropdown-menu-separator
                    ></div>
                  {/snippet}
                </Bits.Separator>
              {/if}
              {@const headingId = `${contentId}-group-${index}`}
              <Bits.Group
                class={slots.group()}
                aria-labelledby={group.heading === undefined ? undefined : headingId}
              >
                {#if group.heading !== undefined}
                  <Bits.GroupHeading>
                    {#snippet child({ props: headingProps })}
                      <div
                        {...headingProps}
                        id={headingId}
                        role="presentation"
                        class={slots.heading()}
                        data-dropdown-menu-group-heading
                      >{group.heading}</div>
                    {/snippet}
                  </Bits.GroupHeading>
                {/if}
                {#each group.items as item (item.id)}
                  <Bits.Item
                    disabled={item.disabled}
                    onSelect={() => choose(item)}
                  >
                    {#snippet child({ props: itemProps })}
                      {#if item.href !== undefined}
                        <a {...itemProps} href={item.href} class={slots.item()}>{@render row(item)}</a>
                      {:else}
                        <div {...itemProps} class={slots.item()}>{@render row(item)}</div>
                      {/if}
                    {/snippet}
                  </Bits.Item>
                {/each}
              </Bits.Group>
            {/each}
          </div>
        </div>
      {/snippet}
    </Bits.Content>
  </Bits.Portal>
</Bits.Root>
