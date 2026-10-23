<!-- A collision-aware, non-modal anchored dialog with a canonical Button trigger. -->
<script lang="ts">
  import { trapScopeTab } from "./core/focus-scope";
  import { portalAppearance } from "./portal-appearance.svelte";
  import type { PopoverContract } from "./core/contracts/popover";
  import type { Snippet } from "svelte";
  import { Popover as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import type { ButtonSize, ButtonVariant } from "./button.variants";
  import { popover as popoverAppearance } from "./popover.variants";

  type Side = "top" | "right" | "bottom" | "left";
  type Align = "start" | "center" | "end";

  interface Props extends PopoverContract {
    /**
     * Optional custom Button content; `triggerLabel` stays its accessible name, so words it shows
     * must appear, in order, in `triggerLabel`, ideally first (WCAG 2.5.3, Label in Name).
     */
    trigger?: Snippet;
    /** Caller-owned Popover content. */
    children?: Snippet;
    /** Extra classes merged onto the anchored surface. */
    class?: string;
    /** Called with the next open state whenever the Popover opens or closes. */
    onopenchange?: (open: boolean) => void;
  }

  let {
    triggerLabel,
    contentLabel,
    open = $bindable(false),
    disabled = false,
    variant = "secondary",
    size = "md",
    side = "bottom",
    align = "start",
    sideOffset = 8,
    collisionPadding = 8,
    trigger,
    children,
    class: className,
    triggerClass,
    onopenchange,
  }: Props = $props();

  const uid = $props.id();
  const contentId = `${uid}-content`;
  let surface: HTMLElement;

  const focusableSelector = [
    "button:not([disabled])",
    "a[href]",
    "input:not([disabled])",
    "select:not([disabled])",
    "textarea:not([disabled])",
    '[tabindex]:not([tabindex="-1"])',
  ].join(",");

  function handleOpenAutoFocus(event: Event): void {
    event.preventDefault();
    (surface.querySelector<HTMLElement>(focusableSelector) ?? surface).focus();
  }
  const portal = portalAppearance(() => open);
</script>

<Bits.Root bind:open onOpenChange={onopenchange}>
  <Bits.Trigger>
    {#snippet child({ props })}
      <Button
        {...props}
        {...portal.anchor}
        data-popover-trigger
        aria-label={triggerLabel}
        {disabled}
        {variant}
        {size}
        class={triggerClass}
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
      trapFocus={true}
      onOpenAutoFocus={handleOpenAutoFocus}
    >
      {#snippet child({ props, wrapperProps })}
        <div {...wrapperProps} {...portal.attributes}>
          <div
            bind:this={surface}
            {@attach trapScopeTab}
            {...props}
            id={contentId}
            data-popover-surface data-ds-focus-scope
            data-avoid-collisions="true"
            data-collision-padding={collisionPadding}
            role="dialog"
            aria-label={contentLabel}
            class={popoverAppearance({ class: className })}
          >
            {@render children?.()}
          </div>
        </div>
      {/snippet}
    </Bits.Content>
  </Bits.Portal>
</Bits.Root>
