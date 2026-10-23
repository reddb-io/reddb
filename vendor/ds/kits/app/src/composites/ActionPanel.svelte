<!-- A named application action region composed from Base Card and SectionHeading. -->
<script lang="ts">
  import type { ActionPanelContract } from "../core/contracts/actionpanel";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    Card,
    SectionHeading,
    type CardPadding,
    type CardVariant,
    type SectionHeadingLevel,
  } from "@reddb-io/design-system/base";
  import { actionPanel } from "./action-panel.variants";

  interface Props extends ActionPanelContract, Omit<HTMLAttributes<HTMLDivElement>, keyof ActionPanelContract | "class" | "title"> {
    /** The panel's canonical links and controls. */
    actions: Snippet;
    /** Optional panel content, rendered between the heading and the actions. */
    children?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the underlying Card. */
    class?: string;
  }

  const {
    title,
    description,
    level = 2,
    variant = "outline",
    padding = "md",
    actions,
    children,
    class: className,
    ...rest
  }: Props = $props();

  const slots = $derived(actionPanel());
  const label = $derived(rest["aria-label"] ?? title);
</script>

<Card
  {...(rest as Record<string, unknown>)}
  data-action-panel
  role="region"
  aria-label={label}
  raised
  {variant}
  {padding}
  class={slots.root({ class: className })}
>
  {#snippet header()}
    <SectionHeading
      {title}
      {description}
      {level}
      size="sm"
      rule={false}
      class={slots.heading()}
    />
  {/snippet}

  <div data-action-panel-content class={slots.content()}>{@render children?.()}</div>

  {#snippet footer()}
    <div data-action-panel-actions class={slots.actions()}>{@render actions()}</div>
  {/snippet}
</Card>
