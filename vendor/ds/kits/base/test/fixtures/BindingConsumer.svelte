<!-- A consumer that binds every native form control the way Svelte apps do. -->
<script lang="ts">
  import { Checkbox, Input, Switch, Textarea } from "@reddb-io/design-system/base";

  let text = $state("initial");
  let notes = $state("first");
  let accepted = $state(false);
  let enabled = $state(true);
</script>

<Input data-bound-input bind:value={text} />
<Textarea data-bound-textarea bind:value={notes} />
<Checkbox data-bound-checkbox label="Accept terms" bind:checked={accepted} />
<Switch data-bound-switch label="Enabled" bind:checked={enabled} />
<output data-text>{text}</output>
<output data-notes>{notes}</output>
<output data-accepted>{String(accepted)}</output>
<output data-enabled>{String(enabled)}</output>
<button type="button" data-reset onclick={() => ((text = "reset"), (accepted = true))}>Reset</button>
