<script lang="ts">
  interface Props {
    failure: "sidebar-without-current" | "bottom-unreachable-by-keyboard";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "sidebar-without-current"}
  <!-- Deliberately broken: a visual selection is not a current-item semantic. -->
  <nav aria-label="Workspace" data-broken-sidebar-navigation>
    <a href="/overview" class="font-medium">Overview</a>
    <a href="/activity">Activity</a>
  </nav>
{:else}
  <!-- Deliberately broken: click-shaped spans never enter the tab order. -->
  <nav aria-label="Primary" data-broken-bottom-navigation>
    <span>Home</span>
    <span>Search</span>
    <span>Account</span>
  </nav>
{/if}
