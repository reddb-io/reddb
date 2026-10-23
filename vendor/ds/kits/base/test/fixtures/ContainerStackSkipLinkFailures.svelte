<script lang="ts">
  interface Props {
    failure: "fixed-spacing" | "unusable-bypass";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "fixed-spacing"}
  <div data-broken-container class="px-4">
    <div data-broken-stack class="flex flex-col gap-2">Fixed against Density.</div>
  </div>
{:else}
  <a data-broken-skip-link class="sr-only" href="#missing-main">Skip to main content</a>
  <main id="content">Main content</main>
{/if}
