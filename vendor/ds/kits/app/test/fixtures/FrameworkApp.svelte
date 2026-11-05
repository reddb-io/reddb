<script lang="ts">
  import * as app from "../../src/index";
  let { name, options = {}, content = true, slotNames = [] }: { name: string; options?: Record<string, any>; content?: boolean; slotNames?: string[] } = $props();
  export function update(next: Record<string, any>) { options = next; }
  const contentSlots = $derived(Object.fromEntries(slotNames.map(key => [key, slot])));
  const Component = $derived(app[name as keyof typeof app] as any);
</script>
{#snippet slot(context?: any)}<span data-provided-slot>Caller detail</span>{#if context?.close}<button type="button" onclick={context.close}>Close via slot</button>{/if}{/snippet}
{#snippet media()}<span>Preview image</span>{/snippet}
{#snippet actions()}<button type="button">Inspect request</button>{/snippet}
{#snippet icon()}<span aria-hidden="true">•</span>{/snippet}
{#if name === "ActionPanel"}
  <Component {...options} {...contentSlots} actions={contentSlots.actions ?? actions}><span>Request details</span></Component>
{:else if name === "CategoryPreview" || name === "ProductFeature"}
  <Component {...options} {...contentSlots} media={contentSlots.media ?? media}><span>Request details</span></Component>
{:else if name === "Incentive"}
  <Component {...options} {...contentSlots} icon={contentSlots.icon ?? icon} />
{:else if name === "CheckoutForm" || name === "ShoppingCart"}
  <Component {...options} {...contentSlots} />
{:else if name === "CategoryFilter" || name === "Filter"}
  <form><Component {...options} {...contentSlots} /></form>
{:else if name === "SplitView"}
  <Component {...options} {...contentSlots}>{#snippet start()}Before{/snippet}{#snippet end()}After{/snippet}</Component>
{:else if name === "SidebarLayout"}
  <Component {...options} {...contentSlots}>{#snippet sidebar({ item, close })}<button onclick={close}>Close context</button><a href="#detail">{item?.label ?? "Navigation"} detail</a>{/snippet}<span>Request details</span></Component>
{:else}
  <Component {...options} {...contentSlots}>{#if content}<span>Request details</span>{/if}</Component>
{/if}
