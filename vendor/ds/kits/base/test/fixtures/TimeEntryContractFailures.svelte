<!-- Deliberately invalid time entries: proof that the contract suites reject
     impossible clock values and ranges whose end precedes their start. -->
<script lang="ts">
  interface Props {
    failure: "impossible-time" | "inverted-range";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "impossible-time"}
  <div data-time-field>
    <label for="broken-hour">Meeting time</label>
    <div>
      <input id="broken-hour" aria-label="Meeting time, hour" value="29" />
      <span aria-hidden="true">:</span>
      <input aria-label="Meeting time, minute" value="75" />
    </div>
    <input type="hidden" name="meetingTime" value="29:75" />
  </div>
{:else}
  <fieldset data-time-range-field>
    <legend>Support window</legend>
    <label for="broken-start">Start time</label>
    <input id="broken-start" type="time" name="supportStart" value="17:00" />
    <label for="broken-end">End time</label>
    <input id="broken-end" type="time" name="supportEnd" value="09:00" />
  </fieldset>
{/if}
