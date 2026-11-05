<script lang="ts">
  import type { SpeedDialContract } from "../core/contracts/speeddial";
  import type { Snippet } from "svelte";
  import { Button, Popover } from "@reddb-io/design-system/base";
  import type { SpeedDialAction } from "./command-surfaces.behavior";
  import { speedDial, type SpeedDialSize } from "./speed-dial.variants";

  type Side = "top" | "right" | "bottom" | "left";
  type Align = "start" | "center" | "end";

  interface Props extends SpeedDialContract {
    /** Called with the new open state whenever the actions are revealed or hidden. */
    onopenchange?: (open: boolean) => void;
    /** Custom content for the trigger Button; `triggerLabel` is shown when omitted. */
    trigger?: Snippet;
    /** Extra classes, merged over the content slot's own, onto the revealed actions surface. */
    class?: string;
  }

  let {
    triggerLabel,
    contentLabel,
    actions = [],
    open = $bindable(false),
    onopenchange,
    disabled = false,
    size = "md",
    side = "top",
    align = "end",
    sideOffset = 8,
    collisionPadding = 8,
    trigger,
    class: className,
  }: Props = $props();

  const slots = $derived(speedDial({ size }));
</script>

<div data-speed-dial>
  <Popover
    {triggerLabel}
    {contentLabel}
    bind:open
    {onopenchange}
    {disabled}
    variant="primary"
    {size}
    {side}
    {align}
    {sideOffset}
    {collisionPadding}
    {trigger}
    class={slots.content({ class: className })}
  >
    {#each actions as action (action.id)}
      <Button
        data-speed-dial-action
        href={action.href}
        disabled={action.disabled}
        variant="ghost"
        {size}
        class={slots.action()}
        onclick={action.href === undefined ? () => action.onselect?.() : undefined}
      >{action.label}</Button>
    {/each}
  </Popover>
</div>
