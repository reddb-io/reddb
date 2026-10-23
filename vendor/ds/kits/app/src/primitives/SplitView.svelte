<!--
  SplitView — a Primitive: it imports no other Kit component. It imports its
  own behavior module, which is a `.ts` file and not a component, so the
  mechanical test in test/taxonomy.test.ts reads it as what it is.

  Two panes and a divider you can move, by pointer or by keyboard. The
  arithmetic lives in split-view.behavior.ts, where it can be tested without a
  browser; what is left here is the part that genuinely needs a DOM.

  The divider is a real `role="separator"` with a tab stop and value bounds, so
  the split is adjustable without a pointer at all — the failure mode of every
  hand-rolled splitter, and the reason this one is in the Kit rather than in
  each application.

  `fraction` is bindable, and every move the user makes is reported through
  `onfractionchange` (ADR 0026): an application that wants to persist the
  split reads it, and one that does not can ignore it entirely.
-->
<script lang="ts">
  import type { SplitViewContract } from "../core/contracts/splitview";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    SPLIT_BOUNDS,
    clampFraction,
    fractionAt,
    fractionForKey,
    percentOf,
    separatorOrientation,
    type SplitOrientation,
  } from "./split-view.behavior";
  import { splitView } from "./split-view.variants";

  interface Props extends SplitViewContract, Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** Called with the new fraction after each pointer or keyboard move. */
    onfractionchange?: (fraction: number) => void;
    /** The leading pane: left, or top. */
    start?: Snippet;
    /** The trailing pane: right, or bottom. */
    end?: Snippet;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  let {
    orientation = "horizontal",
    fraction = $bindable(0.5),
    onfractionchange,
    min = SPLIT_BOUNDS.min,
    max = SPLIT_BOUNDS.max,
    label = "Resize panes",
    start,
    end,
    class: className,
    ...rest
  }: Props = $props();

  let container: HTMLElement | undefined = $state();
  let dragging = $state(false);

  const bounds = $derived({ min, max });
  const position = $derived(clampFraction(fraction, bounds));
  const slots = $derived(splitView({ orientation, dragging }));

  function grab(event: PointerEvent): void {
    dragging = true;
    // Capture keeps the moves coming once the pointer outruns a divider four
    // pixels wide, which it does immediately. Where there is no pointer
    // capture — jsdom, or a browser declining the id — the root handlers keep
    // the drag alive across either pane while the pointer remains in the split.
    try {
      (event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId);
    } catch {
      /* nothing to capture with; the move handler still runs. */
    }
  }

  function move(event: PointerEvent): void {
    if (!dragging || !container) return;
    const box = container.getBoundingClientRect();
    const next =
      orientation === "horizontal"
        ? fractionAt(event.clientX - box.left, box.width, bounds)
        : fractionAt(event.clientY - box.top, box.height, bounds);
    // A move the container cannot measure leaves the divider alone rather than
    // slamming it to an edge.
    if (next === null) return;
    commit(next);
  }

  function commit(next: number): void {
    if (next === fraction) return;
    fraction = next;
    onfractionchange?.(next);
  }

  function release(): void {
    dragging = false;
  }

  function key(event: KeyboardEvent): void {
    const next = fractionForKey(event.key, position, orientation, bounds);
    // Every other key — the other axis included — belongs to the page.
    if (next === null) return;
    event.preventDefault();
    commit(next);
  }
</script>

<div
  bind:this={container}
  {...rest}
  class={slots.root({ class: className })}
  data-orientation={orientation}
  onpointermove={move}
  onpointerup={release}
  onpointercancel={release}
>
  <div class={slots.pane()} style="flex-basis: {percentOf(position)}">
    {@render start?.()}
  </div>

  <!-- A focusable separator is the ARIA window splitter: interactive by role,
       which is why it takes a tab stop and its own key handling. Svelte's a11y
       pass reads `separator` as the static kind — the rule between two blocks
       of text — and warns; the value bounds above are what tell it apart, and
       are also what a screen reader announces when it lands here. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class={slots.divider()}
    data-split-view-divider
    role="separator"
    tabindex="0"
    aria-label={label}
    aria-orientation={separatorOrientation(orientation)}
    aria-valuenow={Math.round(position * 100)}
    aria-valuemin={Math.round(clampFraction(min, bounds) * 100)}
    aria-valuemax={Math.round(clampFraction(max, bounds) * 100)}
    onpointerdown={grab}
    onlostpointercapture={release}
    onkeydown={key}
  ></div>

  <div class={slots.pane()} style="flex-basis: {percentOf(1 - position)}">
    {@render end?.()}
  </div>
</div>
