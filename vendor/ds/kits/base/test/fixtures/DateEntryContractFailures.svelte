<!-- Deliberately invalid date entries: proof that the contract suites reject
     impossible calendar dates and ranges whose end precedes their start. -->
<script lang="ts">
  interface Props {
    failure: "impossible-date" | "inverted-range";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "impossible-date"}
  <div data-date-field>
    <label for="broken-year">Deployment date</label>
    <div>
      <input id="broken-year" aria-label="Deployment date, year" value="2026" />
      <span aria-hidden="true">-</span>
      <input aria-label="Deployment date, month" value="02" />
      <span aria-hidden="true">-</span>
      <input aria-label="Deployment date, day" value="30" />
    </div>
    <input type="hidden" name="deploymentDate" value="2026-02-30" />
  </div>
{:else}
  <fieldset data-date-range-field>
    <legend>Deployment window</legend>
    <label for="broken-start">Start date</label>
    <input id="broken-start" type="date" name="deploymentStart" value="2026-08-20" />
    <label for="broken-end">End date</label>
    <input id="broken-end" type="date" name="deploymentEnd" value="2026-08-10" />
  </fieldset>
{/if}
