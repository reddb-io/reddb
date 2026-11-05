<!-- Wave 5A: ApplicationShell owns <main>; SidebarLayout and nested shells yield to it. -->
<script lang="ts">
  import ApplicationShell from "../../src/composites/ApplicationShell.svelte";
  import SidebarLayout from "../../src/composites/SidebarLayout.svelte";

  interface Props {
    case: "sidebar-in-shell" | "sidebar-forced-main" | "shell-in-shell" | "sidebar-alone" | "sidebar-opt-out" | "sidebar-in-embedded-shell";
  }

  const { case: which }: Props = $props();
</script>

{#snippet aside()}<p>Filters</p>{/snippet}

{#if which === "sidebar-in-shell"}
  <ApplicationShell>
    <SidebarLayout sidebar={aside}><p>Nodes</p></SidebarLayout>
  </ApplicationShell>
{:else if which === "sidebar-forced-main"}
  <ApplicationShell embedded>
    <SidebarLayout sidebar={aside} main mainId="workspace"><p>Nodes</p></SidebarLayout>
  </ApplicationShell>
{:else if which === "shell-in-shell"}
  <ApplicationShell>
    <ApplicationShell mainId="preview"><p>Preview</p></ApplicationShell>
  </ApplicationShell>
{:else if which === "sidebar-in-embedded-shell"}
  <ApplicationShell embedded>
    <SidebarLayout sidebar={aside}><p>Nodes</p></SidebarLayout>
  </ApplicationShell>
{:else if which === "sidebar-alone"}
  <SidebarLayout sidebar={aside}><p>Nodes</p></SidebarLayout>
{:else if which === "sidebar-opt-out"}
  <SidebarLayout sidebar={aside} main={false}><p>Nodes</p></SidebarLayout>
{/if}
