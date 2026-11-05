<script lang="ts">
  import { untrack } from "svelte";
  import SecretValue from "../../src/SecretValue.svelte";
  let { options }: { options: any } = $props();
  let current = $state(untrack(() => options));
  export function update(next: any) {
    current = next;
  }
</script>

<SecretValue {...current} />
