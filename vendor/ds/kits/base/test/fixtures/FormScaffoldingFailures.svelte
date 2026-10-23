<script lang="ts">
  interface Props {
    failure: "unlabelled-control" | "lost-fieldset-group";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "unlabelled-control"}
  <div>
    <span>Email address</span>
    <input id="unlabelled-email" name="email" />
  </div>
{:else}
  <div data-intended-group>
    <span>Contact preferences</span>
    <label for="email-notices">Email notices</label>
    <input id="email-notices" name="notices" type="checkbox" />
  </div>
{/if}
