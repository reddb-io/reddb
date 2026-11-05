<script lang="ts">
  import { untrack } from "svelte";
  import ChangeReview from "../../src/composites/ChangeReview.svelte";
  let { options }: { options: any } = $props();
  let current = $state(untrack(() => options));
  export function update(next: any) {
    current = next;
  }
</script>

<ChangeReview {...current} />
