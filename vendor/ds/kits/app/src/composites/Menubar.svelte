<script lang="ts">
  import type { MenubarContract } from "../core/contracts/menubar";
  import { Menubar as Bits } from "bits-ui";
  import {
    portalAppearance,
    Button,
    Icon,
    dropdownMenuGroups,
    dropdownMenuItems,
    type DropdownMenuItem,
  } from "@reddb-io/design-system/base";
  import type { MenubarMenu } from "./command-surfaces.behavior";
  import { menubar, type MenubarSize } from "./menubar.variants";

  interface Props extends MenubarContract {
    /** Called with the open menu's id whenever a menu opens, closes or changes. */
    onvaluechange?: (value: string) => void;
    /** Extra classes, merged over the root slot's own, onto the menubar element. */
    class?: string;
  }

  let {
    label,
    menus = [],
    value = $bindable(""),
    onvaluechange,
    size = "md",
    loop = true,
    sideOffset = 8,
    collisionPadding = 8,
    class: className,
  }: Props = $props();

  const uid = $props.id();
  const slots = $derived(menubar({ size }));

  function choose(item: DropdownMenuItem): void {
    item.onselect?.();
  }
  const portal = portalAppearance(() => value !== "");
</script>

{#snippet row(item: DropdownMenuItem, hasIcons: boolean)}
  {#if hasIcons}
    <span data-menu-item-icon aria-hidden="true" class={slots.icon()}>
      {#if item.icon !== undefined}<Icon icon={item.icon} {size} aria-hidden="true" />{/if}
    </span>
  {/if}
  {item.label}
{/snippet}

<Bits.Root bind:value onValueChange={onvaluechange} {loop}>
  {#snippet child({ props })}
    <div {...props} {...portal.anchor} data-command-menubar aria-label={label} class={slots.root({ class: className })}>
      {#each menus as menu (menu.id)}
        {@const contentId = `${uid}-${menu.id}-menu`}
        {@const groups = dropdownMenuGroups(menu.items)}
        {@const hasIcons = dropdownMenuItems(menu.items).some((item) => item.icon !== undefined)}
        <Bits.Menu value={menu.id}>
          <Bits.Trigger disabled={menu.disabled}>
            {#snippet child({ props: triggerProps })}
              <Button {...triggerProps} data-menubar-trigger variant="ghost" {size}>
                {menu.label}
              </Button>
            {/snippet}
          </Bits.Trigger>

          <Bits.Portal to={portal.container}>
            <Bits.Content
              id={contentId}
              {sideOffset}
              avoidCollisions={true}
              {collisionPadding}
              loop={true}
            >
              {#snippet child({ props: contentProps, wrapperProps })}
                <div {...wrapperProps} {...portal.attributes}>
                  <div
                    {...contentProps}
                    id={contentId}
                    data-menubar-surface
                    aria-label={`${menu.label} commands`}
                    class={slots.content()}
                  >
                    {#each groups as group, index (index)}
                      {#if index > 0}
                        <Bits.Separator class={slots.separator()} />
                      {/if}
                      {@const headingId = `${contentId}-group-${index}`}
                      <Bits.Group
                        class={slots.group()}
                        aria-labelledby={group.heading === undefined ? undefined : headingId}
                      >
                        {#if group.heading !== undefined}
                          <Bits.GroupHeading id={headingId} class={slots.heading()}>
                            {group.heading}
                          </Bits.GroupHeading>
                        {/if}
                        {#each group.items as item (item.id)}
                          <Bits.Item disabled={item.disabled} onSelect={() => choose(item)}>
                            {#snippet child({ props: itemProps })}
                              {#if item.href !== undefined}
                                <a {...itemProps} href={item.href} class={slots.item()}>{@render row(item, hasIcons)}</a>
                              {:else}
                                <div {...itemProps} class={slots.item()}>{@render row(item, hasIcons)}</div>
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
        </Bits.Menu>
      {/each}
    </div>
  {/snippet}
</Bits.Root>
