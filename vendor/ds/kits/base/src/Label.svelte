<!-- Native control naming; content and the target id remain caller-owned. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLLabelAttributes } from "svelte/elements";
  import { label } from "./label.variants";

  interface Props extends Omit<HTMLLabelAttributes, "children" | "class"> {
    /** The native label element for rare imperative access. */
    ref?: HTMLLabelElement;
    /** Extra classes merged onto the canonical label appearance. */
    class?: string;
    /** The label's content, usually the text naming the control it targets. */
    children?: Snippet;
  }

  let { ref = $bindable(), class: className, children, ...rest }: Props = $props();
</script>

<label {...rest} bind:this={ref} class={label({ class: className })}>
  {@render children?.()}
</label>
