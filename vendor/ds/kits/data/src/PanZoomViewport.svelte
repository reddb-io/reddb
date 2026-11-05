<script module lang="ts">
  export type { PanZoomView } from "./pan-zoom";
  export type { PanZoomControls } from "./pan-zoom";
</script>

<script lang="ts">
  import type { PanZoomViewportContract } from "./core/contracts/panzoomviewport";
  import { onMount, untrack, type Snippet } from "svelte";
  import { Button } from "@reddb-io/design-system/base";
  import {
    checkView,
    fitView,
    zoomView,
    type PanZoomView,
    type PanZoomControls,
  } from "./pan-zoom";
  import { panZoom } from "./pan-zoom.variants";
  import type { VisualizationSize } from "./visualization";
  interface Props extends PanZoomViewportContract {
    /** Reports fit, keyboard, pointer and control changes to the view. */
    onviewchange?: (view: PanZoomView) => void;
    /** Replacement controls receiving the canonical view operations. */
    controls?: Snippet<[PanZoomControls]>;
    /** Unscaled viewport content receiving the current camera transform. */
    overlay?: Snippet<[PanZoomView]>;
    /** Caller-owned drawing. Interactive children retain their native behavior. */
    children: Snippet;
    /** Extra classes on the outer composition. */
    class?: string;
  }
  let {
    label,
    contentWidth,
    contentHeight,
    view = $bindable(),
    onviewchange,
    onnavigate,
    overlay,
    minScale = 0.05,
    maxScale = 8,
    size = "md",
    aspectRatio,
    showControls = true,
    controls,
    zoomInLabel = "Zoom in",
    zoomOutLabel = "Zoom out",
    fitLabel = "Fit",
    instructions = "Drag to pan. Use arrow keys to move, plus or minus to zoom, and Home to fit. Control or Command plus wheel zooms around the pointer.",
    children,
    class: className,
  }: Props = $props();
  const id = $props.id();
  const styles = panZoom();
  const rows = { sm: 6, md: 10, lg: 14 };
  let viewport = $state<HTMLDivElement>();
  let width = $state(0),
    height = $state(0);
  let interacted = false;
  const initialView = untrack(() => view);
  const current = $derived.by(() => {
    fitView(
      { width: contentWidth, height: contentHeight },
      { width: 1, height: 1 },
      minScale,
      maxScale,
    );
    if (
      aspectRatio !== undefined &&
      (!Number.isFinite(aspectRatio) || aspectRatio <= 0)
    )
      throw new Error(
        "PanZoomViewport: aspectRatio must be finite and positive",
      );
    return checkView(view ?? { x: 0, y: 0, scale: 1 }, minScale, maxScale);
  });
  const pointers = new Map<number, { x: number; y: number }>();
  function update(next: PanZoomView, manual = true) {
    if (manual) onnavigate?.();
    view = checkView(next, minScale, maxScale);
    onviewchange?.(view);
  }
  function reset(manual = false) {
    if (width && height)
      update(
        fitView(
          { width: contentWidth, height: contentHeight },
          { width, height },
          minScale,
          maxScale,
        ),
        manual,
      );
  }
  function zoom(factor: number, anchor = { x: width / 2, y: height / 2 }) {
    interacted = true;
    update(zoomView(current, factor, anchor, minScale, maxScale));
  }
  $effect(() => {
    if (width && height && !interacted && !initialView) untrack(reset);
  });
  function point(event: PointerEvent | WheelEvent) {
    const box = viewport!.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  }
  function interactive(target: EventTarget | null) {
    return (
      target instanceof Element &&
      target.closest(
        "button,a,input,textarea,select,[contenteditable=true],[role=button],[role=link]",
      )
    );
  }
  function down(event: PointerEvent) {
    if (event.button !== 0 || interactive(event.target)) return;
    interacted = true;
    event.preventDefault();
    viewport?.focus({ preventScroll: true });
    pointers.set(event.pointerId, point(event));
    viewport?.setPointerCapture(event.pointerId);
  }
  function move(event: PointerEvent) {
    const previous = pointers.get(event.pointerId);
    if (!previous) return;
    const next = point(event),
      other = [...pointers.entries()].find(
        ([id]) => id !== event.pointerId,
      )?.[1];
    if (other) {
      const before = Math.hypot(previous.x - other.x, previous.y - other.y),
        after = Math.hypot(next.x - other.x, next.y - other.y);
      const oldCenter = {
        x: (previous.x + other.x) / 2,
        y: (previous.y + other.y) / 2,
      };
      const newCenter = {
        x: (next.x + other.x) / 2,
        y: (next.y + other.y) / 2,
      };
      const scaled = zoomView(
        current,
        before > 0 ? Math.max(0.01, after / before) : 1,
        oldCenter,
        minScale,
        maxScale,
      );
      update({
        ...scaled,
        x: scaled.x + newCenter.x - oldCenter.x,
        y: scaled.y + newCenter.y - oldCenter.y,
      });
    } else
      update({
        ...current,
        x: current.x + next.x - previous.x,
        y: current.y + next.y - previous.y,
      });
    pointers.set(event.pointerId, next);
  }
  function release(event: PointerEvent) {
    pointers.delete(event.pointerId);
  }
  function revealFocus(event: FocusEvent) {
    if (
      !viewport ||
      event.target === viewport ||
      !(event.target instanceof Element)
    )
      return;
    const box = viewport.getBoundingClientRect(),
      target = event.target.getBoundingClientRect();
    const dx =
      target.left < box.left
        ? box.left - target.left
        : target.right > box.right
          ? box.right - target.right
          : 0;
    const dy =
      target.top < box.top
        ? box.top - target.top
        : target.bottom > box.bottom
          ? box.bottom - target.bottom
          : 0;
    if (dx || dy) {
      interacted = true;
      update({ ...current, x: current.x + dx, y: current.y + dy });
    }
    viewport.scrollLeft = 0;
    viewport.scrollTop = 0;
  }
  onMount(() => {
    const root = viewport!;
    function wheel(event: WheelEvent) {
      if ((!event.ctrlKey && !event.metaKey) || interactive(event.target))
        return;
      event.preventDefault();
      zoom(
        Math.exp(-Math.max(-100, Math.min(100, event.deltaY)) * 0.01),
        point(event),
      );
    }
    root.addEventListener("wheel", wheel, { passive: false });
    return () => {
      root.removeEventListener("wheel", wheel);
      pointers.clear();
    };
  });
  function key(event: KeyboardEvent) {
    if (
      event.target !== event.currentTarget ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey
    )
      return;
    if (event.key === "+" || event.key === "=") zoom(1.2);
    else if (event.key === "-") zoom(1 / 1.2);
    else if (event.key === "Home" || event.key === "0") {
      interacted = false;
      reset(true);
    } else if (
      ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)
    ) {
      interacted = true;
      const distance = Math.min(width, height) / 10;
      update({
        ...current,
        x:
          current.x +
          (event.key === "ArrowLeft"
            ? distance
            : event.key === "ArrowRight"
              ? -distance
              : 0),
        y:
          current.y +
          (event.key === "ArrowUp"
            ? distance
            : event.key === "ArrowDown"
              ? -distance
              : 0),
      });
    } else return;
    event.preventDefault();
  }
</script>

<div data-pan-zoom class={styles.root({ class: className })}>
  <p id={`${id}-instructions`} class="sr-only">{instructions}</p>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions (A named drawing region has documented keyboard pan/zoom and preserves interactive children's native behavior.) -->
  <div
    bind:this={viewport}
    bind:clientWidth={width}
    bind:clientHeight={height}
    class={styles.viewport()}
    role="region"
    aria-label={label}
    aria-describedby={`${id}-instructions`}
    tabindex="0"
    style:height={aspectRatio === undefined
      ? `calc(var(--reddb-spatial-control-height-md) * ${rows[size]})`
      : undefined}
    style:aspect-ratio={aspectRatio}
    style:max-height={`calc(var(--reddb-spatial-control-height-md) * ${rows[size]})`}
    onkeydown={key}
    onpointerdown={down}
    onpointermove={move}
    onpointerup={release}
    onpointercancel={release}
    onlostpointercapture={release}
    onfocusin={revealFocus}
  >
    <div
      data-pan-zoom-content
      class={styles.content()}
      style:width={`${contentWidth}px`}
      style:height={`${contentHeight}px`}
      style:transform={`translate(${current.x}px, ${current.y}px) scale(${current.scale})`}
    >
      {@render children()}
    </div>
    {@render overlay?.(current)}
  </div>
  {#if showControls}
    {#if controls}{@render controls({
        zoomIn: () => zoom(1.2),
        zoomOut: () => zoom(1 / 1.2),
        reset: () => reset(true),
        view: current,
      })}
    {:else}
      <div
        class={styles.controls()}
        role="group"
        aria-label={`${label} controls`}
      >
        <Button
          size="sm"
          variant="secondary"
          disabled={current.scale >= maxScale}
          onclick={() => zoom(1.2)}>{zoomInLabel}</Button
        >
        <Button
          size="sm"
          variant="secondary"
          disabled={current.scale <= minScale}
          onclick={() => zoom(1 / 1.2)}>{zoomOutLabel}</Button
        >
        <span class={styles.status()} aria-live="off"
          >{Math.round(current.scale * 100)}%</span
        >
        <Button
          size="sm"
          variant="ghost"
          onclick={() => {
            interacted = false;
            reset(true);
          }}>{fitLabel}</Button
        >
      </div>
    {/if}
  {/if}
</div>
