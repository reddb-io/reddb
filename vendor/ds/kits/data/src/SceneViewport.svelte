<script module lang="ts">
  export type {
    SceneBounds,
    SceneEngine,
    SceneEngineContext,
    SceneEngineFactory,
  } from "./scene-viewport";
  export type { SceneAnnotation } from "./scene-annotations";
</script>

<script lang="ts">
  import { operationalView } from "./operational.variants";

  import { untrack, type Snippet } from "svelte";
  import { sceneViewport } from "./scene-viewport.variants";
  import { mountSceneAnnotations, sceneAnnotations } from "./scene-annotations";
  import {
    Button,
    quietControl,
    useReducedMotion,
  } from "@reddb-io/design-system/base";
  import PanZoomViewport from "./PanZoomViewport.svelte";
  import { mountSceneCamera, mountSceneEngine } from "./scene-viewport";
  import type { PanZoomView } from "./pan-zoom";
  import type { SceneViewportContract } from "./core/contracts/sceneviewport";
  interface Props extends SceneViewportContract {
    /** Accessible static content, retained if a renderer is unavailable. */
    children: Snippet;
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    contentWidth,
    contentHeight,
    size = "md",
    focus = null,
    annotations = [],
    selected = null,
    onannotationselect,
    follow,
    following = $bindable(false),
    onfollowingchange,
    followLabel = "Follow",
    pauseFollowLabel = "Pause following",
    reducedMotionLabel = "Following is paused for reduced motion.",
    grid = true,
    engine,
    data,
    children,
    class: className,
  }: Props = $props();
  let host = $state<HTMLDivElement>(),
    engineHost = $state<HTMLDivElement>(),
    view = $state<PanZoomView>({ x: 0, y: 0, scale: 1 }),
    error = $state<string | null>(null),
    attempt = $state(0);
  const motion = useReducedMotion();
  const a = sceneViewport();
  const labels = $derived(sceneAnnotations(annotations));
  let labelLayout: ReturnType<typeof mountSceneAnnotations> | undefined;
  function setFollowing(
    next: boolean,
    reason: Parameters<
      NonNullable<SceneViewportContract["onfollowingchange"]>
    >[1],
  ) {
    following = next;
    onfollowingchange?.(next, reason);
  }
  function navigate() {
    if (following) setFollowing(false, "navigate");
  }
  $effect(() => {
    if (host) {
      labelLayout = mountSceneAnnotations(host);
      return () => {
        labelLayout?.destroy();
        labelLayout = undefined;
      };
    }
  });
  $effect(() => {
    labels;
    selected;
    if (view) labelLayout?.update(view);
  });
  let camera = $state<ReturnType<typeof mountSceneCamera>>();
  $effect(() => {
    if (host) {
      camera = mountSceneCamera(host, (next) => (view = next));
      return () => {
        camera?.destroy();
        camera = undefined;
      };
    }
  });
  $effect(() => {
    const reason = camera?.update(
      focus ?? { x: 0, y: 0, width: contentWidth, height: contentHeight },
      follow,
      following,
      motion.current,
    );
    if (reason) untrack(() => setFollowing(false, reason));
  });
  let renderer: ReturnType<typeof mountSceneEngine> | undefined;
  const id = $props.id();
  $effect(() => {
    attempt;
    if (!engineHost || !engine) return;
    error = null;
    renderer = mountSceneEngine(
      engineHost,
      engine,
      label,
      untrack(() => data),
      (message) => (error = message),
    );
    return () => {
      renderer?.destroy();
      renderer = undefined;
    };
  });
  $effect(() => {
    renderer?.update(data);
  });
  const visual = operationalView();
</script>

<div
  bind:this={host}
  data-scene-viewport
  class={visual.scene({ class: className })}
>
  <PanZoomViewport
    {label}
    {contentWidth}
    {contentHeight}
    {size}
    bind:view
    onnavigate={navigate}
    ><div class={visual.sceneContent()}>
      {#if grid}<svg aria-hidden="true" class={visual.overlay()}
          ><defs
            ><pattern {id} width="24" height="24" patternUnits="userSpaceOnUse"
              ><circle
                cx="1"
                cy="1"
                r="1"
                fill="var(--reddb-color-muted)"
              /></pattern
            ></defs
          ><rect width="100%" height="100%" fill={`url(#${id})`} /></svg
        >{/if}
      <div class={visual.staticDrawing()}>{@render children()}</div>
      <div
        bind:this={engineHost}
        data-scene-engine
        aria-hidden="true"
        class={visual.engine()}
      ></div>
    </div>
    {#snippet overlay()}
      <div class={a.annotations()}>
        {#each labels as item (item.id)}
          {#if onannotationselect}<button
              type="button"
              hidden
              data-scene-annotation={item.id}
              data-x={item.x}
              data-y={item.y}
              data-selected={selected === item.id}
              aria-pressed={selected === item.id}
              class={quietControl({ ink: "foreground", class: a.annotation() })}
              onclick={() => onannotationselect?.(item.id)}
              ><span class={a.label()}>{item.label}</span
              >{#if item.description}<span class={a.description()}
                  >{item.description}</span
                >{/if}</button
            >
          {:else}<div
              hidden
              data-scene-annotation={item.id}
              data-x={item.x}
              data-y={item.y}
              data-selected={selected === item.id}
              class={a.annotation()}
            >
              <span class={a.label()}>{item.label}</span
              >{#if item.description}<span class={a.description()}
                  >{item.description}</span
                >{/if}
            </div>{/if}
        {/each}
      </div>
    {/snippet}
  </PanZoomViewport>
  {#if follow !== undefined}<div class={a.followControls()}>
      <Button
        size="sm"
        variant="secondary"
        aria-pressed={following && !motion.current}
        disabled={!follow || motion.current}
        onclick={() => setFollowing(!following, "control")}
        >{following ? pauseFollowLabel : followLabel}</Button
      >{#if motion.current}<span class={a.note()}>{reducedMotionLabel}</span
        >{/if}
    </div>{/if}
  {#if error}<p role="status" class={visual.note()}>{error}</p>
    <Button size="sm" onclick={() => attempt++}>Retry renderer</Button>{/if}
</div>
