<!-- An uncontrolled control inside a Field whose validation state changes around it. -->
<script lang="ts">
  import { Checkbox, Field, Input } from "@reddb-io/design-system/base";

  let error = $state<string | undefined>(undefined);
</script>

<Field label="Email" {error}>
  {#snippet children(control)}
    <Input {...control} name="email" data-uncontrolled-input />
  {/snippet}
</Field>
<Checkbox label="Remember me" {...{ error }} data-uncontrolled-checkbox />
<button type="button" data-validate onclick={() => (error = error ? undefined : "Enter your email.")}>
  Validate
</button>
