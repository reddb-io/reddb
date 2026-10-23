<!-- A perceivable separator whose direction is semantic as well as visual. -->
<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import { divider, type DividerOrientation } from "./divider.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role"> {
    /** Direction of both the separator semantics and its painted rule. */
    orientation?: DividerOrientation;
    /** Extra classes merged onto the canonical rule. */
    class?: string;
  }

  const { orientation = "horizontal", class: className, ...rest }: Props = $props();
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-divider
  role="separator"
  aria-orientation={orientation}
  class={divider({ orientation, class: className })}
></div>
