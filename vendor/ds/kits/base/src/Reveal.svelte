<!-- Native entry motion; server-rendered content remains visible without JavaScript. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { reveal } from "./reveal.variants";
  import { initialRevealState, mountReveal, revealDelay, type RevealDirection } from "./core/reveal";
  export type { RevealDirection } from "./core/reveal";
  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** The edge content enters from. */
    direction?: RevealDirection;
    /** Milliseconds to wait before entering. */
    delay?: number;
    /** Show the entry range while tuning a composition. */
    debug?: boolean;
    /** Extra classes merged onto the primitive. */
    class?: string;
    /** Content that enters the viewport with the reveal. */
    children?: Snippet;
  }
  const { direction = "up", delay = 0, debug = false, class: className, children, ...rest }: Props = $props();
  let element = $state<HTMLDivElement>();
  let motionState = $state(initialRevealState());
  const wait = $derived(revealDelay(delay));
  $effect(() => {
    if (element) return mountReveal(element, direction, wait, next => { motionState = next; });
  });
</script>
<div bind:this={element} {...rest}
  class={reveal({ direction, path: motionState.path, entered: motionState.entered, debug, class: className })}
  style:--reveal-delay={`${wait}ms`}
  style:--reveal-x={motionState.reducedMotion ? "0px" : undefined}
  style:--reveal-y={motionState.reducedMotion ? "0px" : undefined}
  data-reveal data-reveal-path={motionState.path} data-reveal-motion={motionState.reducedMotion ? "fade" : "position-and-fade"}
  data-reveal-direction={direction} data-reveal-delay={wait} data-reveal-debug={debug ? "" : undefined} data-reveal-entered={motionState.entered ? "" : undefined}>
  {@render children?.()}
</div>
