<script lang="ts">
  import { Navbar } from "@reddb-io/design-system/base";
  import {
    SidebarRail,
    shellBrandRegion,
    type SidebarRailItem,
  } from "../../src/index";

  interface Props {
    navbar: boolean;
    rail: boolean;
  }

  const { navbar, rail }: Props = $props();
  const region = $derived(shellBrandRegion({ navbar, rail }));
  const noRailItems: readonly SidebarRailItem[] = [];
</script>

{#snippet brandMark()}
  <a href="/" data-shell-brand-mark>Acme</a>
{/snippet}

{#snippet navbarBrand()}
  <div data-shell-brand-region="navbar">
    {#if region === "navbar"}{@render brandMark()}{/if}
  </div>
{/snippet}

{#snippet railBrand()}
  <div data-shell-brand-region="rail">
    {#if region === "rail"}{@render brandMark()}{/if}
  </div>
{/snippet}

<div data-shell-brand-composition>
  {#if navbar}
    <Navbar label="Product" collapse="expanded" brand={navbarBrand} />
  {/if}

  {#if rail}
    <SidebarRail
      label="Application rail"
      items={noRailItems}
      currentId=""
      top={railBrand}
    />
  {/if}

  <header data-sidebar-panel-header data-shell-brand-region="panel">
    {#if region === "panel"}{@render brandMark()}{/if}
  </header>
</div>
