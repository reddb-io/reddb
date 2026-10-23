<!-- Deliberately broken masks: one removes named content from the accessibility
     tree; the other collapses its clip and removes its control from operation. -->
<script lang="ts">
  interface Props {
    failure: "hidden-name" | "unreachable-control";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "hidden-name"}
  <div data-broken-content-mask aria-hidden="true" style="clip-path: circle(50%)">
    <img src="/ada.png" alt="Ada Lovelace" />
  </div>
{:else}
  <div data-broken-content-mask style="clip-path: circle(0%)">
    <button type="button" disabled tabindex="-1">Open profile</button>
  </div>
{/if}
