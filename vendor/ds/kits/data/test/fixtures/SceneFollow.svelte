<script lang="ts">
  import { untrack } from "svelte";
  import SceneViewport from "../../src/SceneViewport.svelte";
  let { options }: { options: any } = $props();
  let current = $state(untrack(() => options));
  export function update(next: any) {
    current = next;
  }
</script>

<SceneViewport {...current}><p>Static fallback</p></SceneViewport>
