<script lang="ts">
  import type { Component } from "svelte";
  import AspectRatio from "../../src/AspectRatio.svelte";
  import Skeleton from "../../src/Skeleton.svelte";
  import Prose from "../../src/Prose.svelte";
  import Eyebrow from "../../src/Eyebrow.svelte";
  import Kbd from "../../src/Kbd.svelte";
  import Form from "../../src/Form.svelte";
  import Fieldset from "../../src/Fieldset.svelte";
  import SkipLink from "../../src/SkipLink.svelte";
  import Meter from "../../src/Meter.svelte";
  import Progress from "../../src/Progress.svelte";
  import RadialProgress from "../../src/RadialProgress.svelte";
  import Indicator from "../../src/Indicator.svelte";
  import StatusIndicator from "../../src/StatusIndicator.svelte";
  import List from "../../src/List.svelte";
  import GridList from "../../src/GridList.svelte";
  import DescriptionList from "../../src/DescriptionList.svelte";
  import ControlGroup from "../../src/ControlGroup.svelte";
  import ContentMask from "../../src/ContentMask.svelte";
  import SectionHeading from "../../src/SectionHeading.svelte";
  import Statistic from "../../src/Statistic.svelte";
  const adapters: Record<string, Component<any>> = { AspectRatio, Skeleton, Prose, Eyebrow, Kbd, Form, Fieldset, SkipLink, Meter, Progress, RadialProgress, Indicator, StatusIndicator, List, GridList, DescriptionList, ControlGroup, ContentMask, SectionHeading, Statistic };
  let { name, options = {}, content = true, rich = false }: { name: string; options?: Record<string, any>; content?: boolean; rich?: boolean } = $props();
  export function update(next: Record<string, unknown>) { options = next; }
  const Adapter = $derived(adapters[name]!);
</script>
{#snippet marker()}<span aria-label="3 unread">3</span>{/snippet}
{#snippet action()}<button type="button">Details</button>{/snippet}
{#snippet term()}<strong>Requests</strong>{/snippet}
{#snippet detail()}<a href="/detail">View details</a>{/snippet}
{#if name === "Fieldset"}
  <Fieldset {...options}>
    {#snippet children(control)}<input {...control} name="choice" aria-label="Choice" value="chosen" />{/snippet}
  </Fieldset>
{:else if name === "Indicator"}
  <Indicator {...options} {marker}><button type="button">Inbox</button></Indicator>
{:else if name === "SectionHeading"}
  {#if rich}<SectionHeading {...options} actions={action} />{:else}<SectionHeading {...options} />{/if}
{:else if name === "DescriptionList"}
  <DescriptionList {...options} items={rich ? [{ term, detail }] : options.items} />
{:else if name === "Form"}
  <Form {...options}><input name="query" aria-label="Query" value="original" required /><button type="submit">Submit</button><button type="reset">Reset</button></Form>
{:else if name === "List" || name === "GridList"}
  <Adapter {...options}>
    {#if options.list === false}<div>Alpha</div><div>Beta</div>{:else}<li>Alpha</li><li>Beta</li>{/if}
  </Adapter>
{:else if content}
  <Adapter {...options}><span data-test-content>Content</span></Adapter>
{:else}
  <Adapter {...options} />
{/if}
