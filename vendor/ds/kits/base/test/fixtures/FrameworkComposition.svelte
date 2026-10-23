<script lang="ts">
  import type { Component } from "svelte";
  import Link from "../../src/Link.svelte";
  import Alert from "../../src/Alert.svelte";
  import Notification from "../../src/Notification.svelte";
  import EmptyState from "../../src/EmptyState.svelte";
  import MediaObject from "../../src/MediaObject.svelte";
  import ListContainer from "../../src/ListContainer.svelte";
  import Breadcrumbs from "../../src/Breadcrumbs.svelte";
  import BrowserMockup from "../../src/BrowserMockup.svelte";
  import WindowMockup from "../../src/WindowMockup.svelte";
  import PhoneMockup from "../../src/PhoneMockup.svelte";
  let { name, options = {}, content = true, rich = false }: { name: string; options?: Record<string, any>; content?: boolean; rich?: boolean } = $props();
  const adapters: Record<string, Component<any>> = { Link, Alert, Notification, BrowserMockup, WindowMockup, PhoneMockup };
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, unknown>) { options = next; }
</script>
{#snippet media()}<span data-slot="media">M</span>{/snippet}
{#snippet body()}<a href="/detail" data-slot="content">Details</a>{/snippet}
{#snippet actions()}<button type="button">Create</button>{/snippet}
{#snippet header()}<strong>Custom title</strong>{/snippet}
{#if name === "EmptyState"}
  <EmptyState {...options} media={rich ? media : undefined} actions={rich ? actions : undefined} />
{:else if name === "MediaObject"}
  <MediaObject {...options} {media}>{#snippet children()}{@render body()}{/snippet}</MediaObject>
{:else if name === "ListContainer"}
  <ListContainer {...options} header={rich ? header : undefined} footer={rich ? actions : undefined}>
    {#snippet children()}<ul><li>Destination</li></ul>{/snippet}
  </ListContainer>
{:else if name === "Breadcrumbs"}
  <Breadcrumbs {...options} items={options.items ?? []} />
{:else if content}
  <Adapter {...options}>{#snippet children()}<span data-slot="body">Content</span>{/snippet}</Adapter>
{:else}
  <Adapter {...options} />
{/if}
