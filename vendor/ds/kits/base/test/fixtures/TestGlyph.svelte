<script lang="ts">
  import type { SVGAttributes } from "svelte/elements";

  interface Props extends SVGAttributes<SVGSVGElement> {
    size?: string | number;
    color?: string;
    strokeWidth?: string | number;
  }

  let { size = 24, color = "currentColor", strokeWidth = 2, ...rest }: Props = $props();
</script>

<svg
  {...rest}
  data-test-glyph
  width={size}
  height={size}
  color={color}
  stroke={color}
  stroke-width={strokeWidth}
  viewBox="0 0 24 24"
></svg>
