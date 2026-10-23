<!-- Native submission scaffold; fields, actions, and business content stay caller-owned. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLFormAttributes } from "svelte/elements";
  import { form } from "./form.variants";

  interface Props extends Omit<HTMLFormAttributes, "children" | "class"> {
    /** The native form element for FormData or imperative submission. */
    ref?: HTMLFormElement;
    /** Extra classes for a consumer's local arrangement. */
    class?: string;
    /** The fields, actions and other content that make up the form. */
    children?: Snippet;
  }

  let { ref = $bindable(), class: className, children, ...rest }: Props = $props();
</script>

<form {...rest} bind:this={ref} class={form({ class: className })}>
  {@render children?.()}
</form>
