<script lang="ts">
  import { untrack } from "svelte";
  import EntityJourney from "../../src/composites/EntityJourney.svelte";
  let { options }: { options: any } = $props();
  let current = $state(untrack(() => options));
  export function update(next: any) {
    current = next;
  }
</script>

<EntityJourney {...current} />
