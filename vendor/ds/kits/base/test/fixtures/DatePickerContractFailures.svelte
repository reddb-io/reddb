<!-- Deliberately desynchronized picker state: the field and calendar publish
     different dates, proving the picker suites reject split ownership. -->
<script lang="ts">
  interface Props {
    failure: "date-picker-desynchronization" | "date-range-picker-desynchronization";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "date-picker-desynchronization"}
  <div data-date-picker>
    <div data-date-field>
      <input type="hidden" name="deploymentDate" value="2026-06-10" />
    </div>
    <div data-calendar>
      <button type="button" data-bits-day data-value="2026-06-12" data-selected>12</button>
    </div>
  </div>
{:else}
  <fieldset data-date-range-picker>
    <input type="hidden" name="deploymentStart" value="2026-06-10" />
    <input type="hidden" name="deploymentEnd" value="2026-06-12" />
    <div data-range-calendar>
      <button type="button" data-bits-day data-value="2026-06-15" data-range-start>15</button>
      <button type="button" data-bits-day data-value="2026-06-18" data-range-end>18</button>
    </div>
  </fieldset>
{/if}
