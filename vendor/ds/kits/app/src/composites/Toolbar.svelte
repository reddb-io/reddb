<script lang="ts">
  import type { ToolbarContract } from "../core/contracts/toolbar";
  import { Toolbar as Bits } from "bits-ui";
  import { Button, Link, quietControl } from "@reddb-io/design-system/base";
  import type { ToolbarItem } from "./command-surfaces.behavior";
  import { toolbar, type ToolbarSize } from "./toolbar.variants";

  type Orientation = "horizontal" | "vertical";

  interface Props extends ToolbarContract {
    /** Extra classes, merged over the root slot's own, onto the toolbar element. */
    class?: string;
  }

  let {
    label,
    items = [],
    orientation = "horizontal",
    size = "md",
    loop = true,
    class: className,
  }: Props = $props();

  const slots = $derived(toolbar({ orientation, size }));
</script>

<Bits.Root {orientation} {loop}>
  {#snippet child({ props })}
    <div
      {...props}
      data-command-toolbar
      aria-label={label}
      aria-orientation={orientation}
      class={slots.root({ class: className })}
    >
      {#each items as item (item.id)}
        {@const href = item.href}
        {#if href !== undefined && !item.disabled}
          <Bits.Link>
            {#snippet child({ props: linkProps })}
              <Link
                {...linkProps}
                data-toolbar-item
                {href}
                class={quietControl({ class: slots.item({ class: "rounded-md no-underline" }) })}
              >{item.label}</Link>
            {/snippet}
          </Bits.Link>
        {:else}
          <Bits.Button disabled={item.disabled}>
            {#snippet child({ props: buttonProps })}
              <Button
                {...buttonProps}
                data-toolbar-item
                disabled={item.disabled}
                variant="ghost"
                {size}
                class={slots.item()}
                onclick={() => item.onselect?.()}
              >{item.label}</Button>
            {/snippet}
          </Bits.Button>
        {/if}
      {/each}
    </div>
  {/snippet}
</Bits.Root>
