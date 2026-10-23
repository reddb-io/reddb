<script lang="ts">
  interface Props {
    failure: "density-frozen-shell" | "sidebar-without-main";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "density-frozen-shell"}
  <div data-broken-application-shell class="flex min-h-screen flex-col p-4">
    <header>Product navigation</header>
    <main>Workspace</main>
  </div>
{:else}
  <div data-broken-sidebar-layout class="grid grid-cols-2 gap-4">
    <aside aria-label="Workspace navigation">Navigation</aside>
    <div data-broken-sidebar-main>Workspace</div>
  </div>
{/if}
