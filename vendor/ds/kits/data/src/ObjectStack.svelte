<script lang="ts">
  import { sceneGeometry, stackItems } from "./isometric";
  import { seriesColor, tint, mutedInk } from "./visualization";
  import { operationalView } from "./operational.variants";
  import type { ObjectStackContract } from "./core/contracts/objectstack";
  interface Props extends ObjectStackContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    count,
    maxVisible = 8,
    series = 1,
    class: className,
  }: Props = $props();
  const stack = $derived(stackItems(count, maxVisible)),
    scene = $derived(sceneGeometry(stack.items));
  const visual = operationalView();
</script>

<figure data-object-stack class={visual.figure({ class: className })}>
  {#if stack.shown > 0}<svg
      viewBox={scene.viewBox}
      role="img"
      aria-label={`${label}: ${count}`}
      class={visual.svg()}
      ><title>{label}: {count}</title>{#each scene.boxes as box (box.id)}<g
          ><polygon
            points={box.left}
            fill={tint(seriesColor(series), 25)}
            stroke={mutedInk}
          /><polygon
            points={box.right}
            fill={tint(seriesColor(series), 40)}
            stroke={mutedInk}
          /><polygon
            points={box.top}
            fill={tint(seriesColor(series), 15)}
            stroke={mutedInk}
          /></g
        >{/each}</svg
    >
  {/if}
  <figcaption class={visual.text()}>
    {label}:
    <span class={visual.count()}>{count}</span>{#if count > stack.shown}
      · {stack.shown} representatives shown{/if}
  </figcaption>
</figure>
