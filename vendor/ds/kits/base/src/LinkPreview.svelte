<!-- A native link with a collision-aware, non-focusable preview surface. -->
<script lang="ts">
  import { portalAppearance } from "./portal-appearance.svelte";
  import type { LinkPreviewContract } from "./core/contracts/linkpreview";
  import type { Snippet } from "svelte";
  import type { HTMLAnchorAttributes } from "svelte/elements";
  import { LinkPreview as Bits } from "bits-ui";
  import { link } from "./link.variants";
  import { linkPreview as linkPreviewAppearance } from "./link-preview.variants";

  interface Props
    extends LinkPreviewContract, Omit<
      HTMLAnchorAttributes,
      "aria-controls" | "aria-expanded" | "aria-haspopup" | "children" | "class" | "href" | "role"
    > {
    /** Called with the next open state (ADR 0026's open dialect). */
    onopenchange?: (open: boolean) => void;
    /** Optional custom link content. */
    trigger?: Snippet;
    /** Caller-owned preview content. */
    children?: Snippet;
    /** Extra classes merged onto the preview surface. */
    class?: string;
  }

  let {
    href,
    label,
    previewLabel,
    open = $bindable(false),
    onopenchange,
    openDelay = 700,
    closeDelay = 300,
    side = "top",
    align = "center",
    sideOffset = 8,
    collisionPadding = 8,
    trigger,
    children,
    class: className,
    triggerClass,
    ...rest
  }: Props = $props();

  const uid = $props.id();
  const contentId = `${uid}-content`;
  const portal = portalAppearance(() => open);
</script>

<Bits.Root bind:open onOpenChange={onopenchange} {openDelay} {closeDelay}>
  <Bits.Trigger>
    {#snippet child({ props })}
      <!-- svelte-ignore a11y_no_redundant_roles (Bits assigns role=button; the override preserves native link semantics.) -->
      <a
        {...portal.anchor}
        {...(rest as Record<string, unknown>)}
        {...props}
        {href}
        data-link-preview-trigger
        role="link"
        class={link({ class: triggerClass })}
      >
        {#if trigger}{@render trigger()}{:else}{label}{/if}
      </a>
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
    >
      {#snippet child({ props, wrapperProps })}
        <div {...wrapperProps} {...portal.attributes}>
          <div
            {...props}
            id={contentId}
            data-link-preview-surface
            data-avoid-collisions="true"
            data-collision-padding={collisionPadding}
            role="dialog"
            aria-label={previewLabel}
            class={linkPreviewAppearance({ class: className })}
          >
            {@render children?.()}
          </div>
        </div>
      {/snippet}
    </Bits.Content>
  </Bits.Portal>
</Bits.Root>
