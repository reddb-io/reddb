<script lang="ts">
  import type { Component } from "svelte";
  import Icon from "../../src/Icon.svelte";
  import Image from "../../src/Image.svelte";
  import Logo from "../../src/Logo.svelte";
  import Card from "../../src/Card.svelte";
  import TestGlyph from "./TestGlyph.svelte";
  let { name, options = {}, rich = false, glyph = false }: { name: string; options?: Record<string, any>; rich?: boolean; glyph?: boolean } = $props();
  const adapters: Record<string, Component<any>> = { Icon, Image, Logo };
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, unknown>) { options = next; }
</script>
{#snippet header()}<strong>Custom header</strong>{/snippet}
{#snippet media()}<span>Custom media</span>{/snippet}
{#snippet footer()}<button type="button">Details</button>{/snippet}
{#if name === "Card"}
  <Card {...options} icon={glyph ? TestGlyph : undefined} header={rich ? header : undefined} media={rich ? media : undefined} {footer}><span>Card content</span></Card>
{:else}
  <Adapter {...options} {...(name === "Icon" && glyph ? { icon: TestGlyph } : {})} />
{/if}
