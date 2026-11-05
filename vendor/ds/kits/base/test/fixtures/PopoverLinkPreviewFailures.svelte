<!-- Deliberately broken public DOM fixtures for anchored-overlay regressions. -->
<script lang="ts">
  interface Props {
    failure: "viewport-escape" | "permanent-focus-trap";
  }

  const { failure }: Props = $props();
  let open = $state(false);

  function trap(event: KeyboardEvent): void {
    if (event.key !== "Tab") return;
    event.preventDefault();
    (event.currentTarget as HTMLElement).querySelector<HTMLElement>("button")?.focus();
  }
</script>

{#if failure === "viewport-escape"}
  <a href="/clusters" data-link-preview-trigger>Clusters</a>
  <div
    data-broken-overlay
    style="position: fixed; left: 1025px; width: 240px;"
  >
    A preview rendered beyond a 1024px viewport
  </div>
{:else}
  <button type="button" data-popover-trigger onclick={() => (open = true)}>Open actions</button>
  {#if open}
    <div
      data-broken-overlay
      role="dialog"
      aria-label="Trapped actions"
      tabindex="-1"
      onkeydown={trap}
    >
      <button type="button">Only trapped action</button>
    </div>
  {/if}
{/if}
