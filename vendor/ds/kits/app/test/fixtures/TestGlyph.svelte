<script lang="ts">
  import type { IconGlyphProps } from "@reddb-io/design-system/base";

  let { size, color, strokeWidth, ...rest }: IconGlyphProps = $props();
</script>

<svg {...rest} width={size} height={size} {color} stroke={color} stroke-width={strokeWidth}>
  <path d="M4 4h16v16H4z" />
</svg>
