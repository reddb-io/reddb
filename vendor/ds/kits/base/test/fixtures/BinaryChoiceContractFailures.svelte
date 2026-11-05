<script lang="ts">
  interface Props {
    failure: "unlabelled-checkbox" | "unshared-radio-name";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "unlabelled-checkbox"}
  <input type="checkbox" name="telemetry" />
{:else}
  <fieldset>
    <legend>Release channel</legend>
    <label><input type="radio" name="stable-channel" value="stable" /> Stable</label>
    <label><input type="radio" name="preview-channel" value="preview" /> Preview</label>
  </fieldset>
{/if}
