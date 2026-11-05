<script lang="ts">
  import { untrack } from "svelte";
  import SceneViewport from "../../src/SceneViewport.svelte";
  import type { SceneEngineFactory } from "../../src/scene-viewport";
  let {
    options,
  }: {
    options: {
      label: string;
      contentWidth: number;
      contentHeight: number;
      engine: SceneEngineFactory;
      data: unknown;
    };
  } = $props();
  let data = $state(untrack(() => options.data));
</script>

<button data-update onclick={() => (data = { revision: 2 })}
  >Update scene</button
>
<SceneViewport {...options} {data}><p>Static content</p></SceneViewport>
