<!-- Deliberately incomplete specialized entries: proof that the suites catch
     hidden file feedback and an OTP surface that cannot accept a pasted code. -->
<script lang="ts">
  interface Props {
    failure: "hidden-file-name" | "broken-otp-paste";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "hidden-file-name"}
  <label for="broken-file">Upload evidence</label>
  <input id="broken-file" type="file" name="evidence" />
{:else}
  <fieldset>
    <legend>Verification code</legend>
    {#each Array(6) as _, index}
      <input
        aria-label={`Digit ${index + 1} of 6`}
        inputmode="numeric"
        maxlength="1"
        onpaste={(event) => event.preventDefault()}
      />
    {/each}
  </fieldset>
{/if}
