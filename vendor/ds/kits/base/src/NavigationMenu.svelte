<script lang="ts">
  import type { NavigationMenuContract } from "./core/contracts/navigationmenu";
  import { NavigationMenu as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import {
    isNavigationMenuSection,
    type NavigationMenuEntry,
    type NavigationMenuLink,
  } from "./navigation-menu.behavior";
  import {
    navigationMenu,
    type NavigationMenuOrientation,
    type NavigationMenuSize,
  } from "./navigation-menu.variants";
  import { popover as popoverAppearance } from "./popover.variants";
  import { quietControl } from "./quiet-control.variants";

  interface Props extends NavigationMenuContract {
    /** Called with the open item's id ("" when none) after it changes (ADR 0026's value dialect). */
    onvaluechange?: (value: string) => void;
    /** Extra classes, merged onto each section's anchored surface, not the landmark. */
    class?: string;
  }

  let {
    label,
    items = [],
    value = $bindable(""),
    onvaluechange,
    orientation = "horizontal",
    size = "md",
    delayDuration = 200,
    skipDelayDuration = 300,
    class: className,
  }: Props = $props();

  const slots = $derived(navigationMenu({ orientation, size }));

  function choose(link: NavigationMenuLink): void {
    link.onselect?.();
  }
</script>

<Bits.Root bind:value onValueChange={onvaluechange} {orientation} {delayDuration} {skipDelayDuration}>
  {#snippet child({ props: rootProps })}
    <nav
      {...rootProps}
      aria-label={label}
      data-navigation-menu-root
      class={slots.root()}
    >
      <Bits.List class={slots.list()}>
        {#each items as item (item.id)}
          <Bits.Item value={item.id} openOnHover={false}>
            {#if isNavigationMenuSection(item)}
              <Bits.Trigger>
                {#snippet child({ props })}
                  <Button
                    {...props}
                    data-navigation-menu-control
                    data-navigation-menu-trigger
                    variant="ghost"
                    {size}
                    class={slots.control()}
                  >{item.label}</Button>
                {/snippet}
              </Bits.Trigger>
              <Bits.Content
                data-navigation-menu-surface
                aria-label={item.label}
                class={popoverAppearance({ class: slots.content({ class: className }) })}
              >
                <ul class={slots.links()}>
                  {#each item.links as link (link.id)}
                    <li>
                      <Bits.Link active={link.active} onSelect={() => choose(link)}>
                        {#snippet child({ props })}
                          <a {...props} href={link.href} class={quietControl({ ink: "foreground", class: slots.link() })}>
                            <span>{link.label}</span>
                            {#if link.description !== undefined}
                              <span class={slots.description()}>{link.description}</span>
                            {/if}
                          </a>
                        {/snippet}
                      </Bits.Link>
                    </li>
                  {/each}
                </ul>
              </Bits.Content>
              {#if value !== item.id}
                <!-- Bits UI mounts a section's surface only while it is open, so a closed
                     section's destinations would exist only after a click. They stay in the
                     served document as a `hidden` list: crawlers and no-script readers find every
                     link, while `hidden` keeps them out of the tab order and the accessibility
                     tree until the real surface takes over on open. -->
                <ul hidden data-navigation-menu-closed-links={item.id}>
                  {#each item.links as link (link.id)}
                    <li>
                      <a href={link.href} aria-current={link.active ? "page" : undefined}>
                        {link.label}{#if link.description !== undefined}<span> — {link.description}</span>{/if}
                      </a>
                    </li>
                  {/each}
                </ul>
              {/if}
            {:else}
              <Bits.Link active={item.active} onSelect={() => choose(item)}>
                {#snippet child({ props })}
                  <a
                    {...props}
                    href={item.href}
                    data-navigation-menu-control
                    aria-current={item.active ? "page" : undefined}
                    class={quietControl({ ink: "foreground", class: slots.control() })}
                  >{item.label}</a>
                {/snippet}
              </Bits.Link>
            {/if}
          </Bits.Item>
        {/each}
      </Bits.List>
    </nav>
  {/snippet}
</Bits.Root>
