<script module lang="ts">
  export type { IsometricItem } from "./isometric";
</script>

<script lang="ts">
  import { operationalView } from "./operational.variants";

  import SceneViewport from "./SceneViewport.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { isometricView } from "./isometric";
  import { seriesColor, tint, mutedInk, ink } from "./visualization";
  import type { IsometricSceneContract } from "./core/contracts/isometricscene";
  interface Props extends IsometricSceneContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    items,
    selected = null,
    focused = null,
    layer = null,
    interactive = false,
    size = "md",
    onselectedchange,
    class: className,
  }: Props = $props();
  const drawing = $derived(isometricView(items, layer, focused));
  const scene = $derived(drawing.scene),
    focus = $derived(drawing.focus);
  const visual = operationalView();
</script>

<figure data-isometric-scene class={visual.sceneFigure({ class: className })}>
  <figcaption class={visual.caption()}>
    {label}
  </figcaption>
  <SceneViewport
    {label}
    contentWidth={scene.width}
    contentHeight={scene.height}
    {focus}
    {size}
    ><svg
      width={scene.width}
      height={scene.height}
      viewBox={scene.viewBox}
      role="group"
      aria-label={label}
      ><title>{label}</title>{#each scene.boxes as box (box.id)}<Mark
          id={box.id}
          label={box.label}
          selected={selected ?? undefined}
          onselect={interactive ? onselectedchange : undefined}
          ><polygon
            points={box.left}
            fill={tint(seriesColor(box.series), 25)}
            stroke={selected === box.id ? ink : mutedInk}
          /><polygon
            points={box.right}
            fill={tint(seriesColor(box.series), 40)}
            stroke={selected === box.id ? ink : mutedInk}
          /><polygon
            points={box.top}
            fill={tint(seriesColor(box.series), selected === box.id ? 35 : 15)}
            stroke={selected === box.id ? ink : mutedInk}
          /></Mark
        >{/each}</svg
    ></SceneViewport
  >
  <ul class={visual.entityList()} aria-label={`${label} entities`}>
    {#each scene.boxes as box (box.id)}<li>
        {box.label}{box.layer ? `: ${box.layer}` : ""}
      </li>{/each}
  </ul>
</figure>
