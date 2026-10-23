<script lang="ts">
  import { sceneGeometry } from "./isometric";
  import { seriesColor, tint, mutedInk } from "./visualization";
  import { operationalView } from "./operational.variants";
  import type { IsometricBlockContract } from "./core/contracts/isometricblock";
  interface Props extends IsometricBlockContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    width = 4,
    depth = 3,
    height = 1,
    series = 1,
    class: className,
  }: Props = $props();
  const scene = $derived(
      sceneGeometry([{ id: "block", label, x: 0, y: 0, width, depth, height }]),
    ),
    s = operationalView();
</script>

<svg
  data-isometric-block
  viewBox={scene.viewBox}
  role="img"
  aria-label={label}
  class={s.svg({ class: className })}
  ><title>{label}</title>{#each scene.boxes as box (box.id)}<polygon
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
    />{/each}</svg
>
