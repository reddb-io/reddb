<script lang="ts">
  import { SplitView } from "@reddb-io/design-system/app";
</script>

<SplitView fraction={0.4} class="h-64" label="Resize navigation and workspace">
  {#snippet start()}
    <nav aria-label="Project">Files</nav>
  {/snippet}
  {#snippet end()}
    <SplitView
      orientation="vertical"
      fraction={0.6}
      class="h-full"
      label="Resize editor and console"
    >
      {#snippet start()}<main>Editor</main>{/snippet}
      {#snippet end()}<aside aria-label="Console">Console</aside>{/snippet}
    </SplitView>
  {/snippet}
</SplitView>
