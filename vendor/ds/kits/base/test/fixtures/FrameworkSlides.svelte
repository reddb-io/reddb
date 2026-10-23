<script lang="ts">
  import type { Component } from "svelte";
  import SlideFrame from "../../src/SlideFrame.svelte";
  import TitleSlide from "../../src/TitleSlide.svelte";
  import SectionSlide from "../../src/SectionSlide.svelte";
  import TwoColumnSlide from "../../src/TwoColumnSlide.svelte";
  import FigureSlide from "../../src/FigureSlide.svelte";
  import QuoteSlide from "../../src/QuoteSlide.svelte";
  import BulletsSlide from "../../src/BulletsSlide.svelte";
  import Deck from "../../src/Deck.svelte";
  const adapters: Record<string, Component<any>> = { TitleSlide, SectionSlide, QuoteSlide, BulletsSlide };
  let { name, options = {} }: { name: string; options?: Record<string, any> } = $props();
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, unknown>) { options = next; }
</script>
{#if name === "SlideFrame"}
  <SlideFrame {...options} label={options.label}><a href="/report">Read report</a></SlideFrame>
{:else if name === "Deck"}
  <Deck {...options} label={options.label}><SlideFrame label="Nested slide"><a href="/report">Read report</a></SlideFrame></Deck>
{:else if name === "TwoColumnSlide"}
  <TwoColumnSlide {...options} title={options.title}>
    {#snippet start()}<h3>Before</h3><p>Manual review</p>{/snippet}
    {#snippet end()}<h3>After</h3><a href="/report">Read report</a>{/snippet}
  </TwoColumnSlide>
{:else if name === "FigureSlide"}
  <FigureSlide {...options} title={options.title}>
    {#snippet figure()}<table><caption>Replicas</caption><tbody><tr><th scope="row">api-01</th><td>Ready</td></tr></tbody></table>{/snippet}
  </FigureSlide>
{:else}
  <Adapter {...options} />
{/if}
