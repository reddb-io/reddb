<!-- A named arrangement boundary for related caller-owned controls. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    controlGroup,
    type ControlGroupOrientation,
  } from "./control-group.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "children" | "class" | "role"> {
    /** The accessible name for the related controls. */
    label: string;
    /** Visual flow without changing keyboard order. */
    orientation?: ControlGroupOrientation;
    /** The group element for rare imperative access. */
    ref?: HTMLDivElement;
    /** Extra classes merged onto the group. */
    class?: string;
    /** The related controls to arrange together; `label` names them as one group. */
    children?: Snippet;
  }

  let {
    label,
    orientation = "horizontal",
    ref = $bindable(),
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<div
  {...rest}
  bind:this={ref}
  role="group"
  aria-label={label}
  data-orientation={orientation}
  class={controlGroup({ orientation, class: className })}
>
  {@render children?.()}
</div>
