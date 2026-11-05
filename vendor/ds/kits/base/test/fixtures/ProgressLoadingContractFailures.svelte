<script lang="ts">
  let { failure }: { failure: "missing-progress-value" | "silent-spinner" } = $props();
</script>

{#if failure === "missing-progress-value"}
  <!-- The fill says 72% to an eye, but the progressbar exposes no value. -->
  <div data-broken-progress role="progressbar" aria-label="Deployment progress">
    <div data-broken-progress-fill style="width: 72%"></div>
  </div>
{:else}
  <!-- Motion says “loading” to an eye; nothing announces that state. -->
  <div data-broken-loading>
    <svg data-broken-spinner viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="9" stroke-width="3" />
    </svg>
  </div>
{/if}
