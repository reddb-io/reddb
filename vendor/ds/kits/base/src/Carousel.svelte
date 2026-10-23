<!-- A named slide set with canonical controls and opt-in pausable rotation. -->
<script module lang="ts">
  import type { Snippet } from "svelte";
  import type { CarouselSlideContract } from "./core/carousel";
  import type { ImageAsset } from "./image.variants";

  export interface CarouselSlide extends CarouselSlideContract {
    /** Visible meaning included in the slide's accessible position label. */
    label: string;
    /** Caller-owned slide content. Omitted, the slide renders its `image`. */
    content?: Snippet;
    /**
     * The slide as an image the app's pipeline produced, rendered through the Base Image (lazy
     * unless it opens the Carousel with `priority`); ignored when `content` is given.
     */
    image?: ImageAsset;
  }

  export interface CarouselViewportContext {
    /** Slides in caller-owned document order. */
    slides: readonly CarouselSlide[];
    /** Current zero-based index; zero for an empty carousel. */
    active: number;
    /** Whether automatic rotation is running. */
    playing: boolean;
    /** Initial slide index, used to prioritize its image. */
    firstIndex: number;
  }
</script>

<script lang="ts">
  import type { CarouselContract } from "./core/contracts/carousel";
  import { untrack } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import Button from "./Button.svelte";
  import Image from "./Image.svelte";
  import { warnDeprecated } from "./deprecation";
  import { carousel } from "./carousel.variants";
  import { useReducedMotion } from "./reduced-motion.svelte";

  interface Props extends CarouselContract, Omit<HTMLAttributes<HTMLDivElement>, "children" | "class" | "onchange"> {
    /** Caller-owned slides in stable document order. */
    slides: readonly CarouselSlide[];
    /** Replaces the default slide drawing while keeping canonical navigation and rotation. The renderer owns slide semantics, inactive content exclusion and image priority. */
    viewport?: Snippet<[CarouselViewportContext]>;
    /** Called with the selected zero-based index after a user or timer selects a slide. */
    onvaluechange?: (index: number) => void;
    /** @deprecated Renamed `onvaluechange` (ADR 0026); removed next release. */
    onchange?: (index: number) => void;
    /** Extra classes merged onto the carousel region. */
    class?: string;
  }

  let {
    label,
    slides,
    viewport,
    initialIndex = 0,
    autoplay = false,
    interval = 5_000,
    onvaluechange,
    onchange,
    priority = false,
    class: className,
    viewportClass,
    slideClass,
    controlsClass,
    ...rest
  }: Props = $props();

  const reducedMotion = useReducedMotion();
  let requestedIndex = $state(untrack(() => Math.max(0, Math.trunc(initialIndex) || 0)));
  const active = $derived(Math.max(0, Math.min(requestedIndex, slides.length - 1)));
  const firstIndex = untrack(() => active);
  let rotationRequested = $state(untrack(() => autoplay && slides.length > 1));
  const isPlaying = $derived(rotationRequested && !reducedMotion.current);
  const styles = carousel();

  function select(index: number): void {
    if (slides.length < 1) return;
    requestedIndex = (index + slides.length) % slides.length;
    onvaluechange?.(requestedIndex);
    onchange?.(requestedIndex);
  }

  $effect(() => {
    if (onchange) warnDeprecated("Carousel", "onchange", "onvaluechange");
  });

  function pauseForInteraction(event: FocusEvent | MouseEvent): void {
    if (!autoplay || !isPlaying) return;
    const target = event.target;
    if (target instanceof Element && target.closest("[data-carousel-rotation]")) return;
    rotationRequested = false;
  }

  $effect(() => {
    if (!isPlaying || slides.length < 2) return;
    const timer = window.setInterval(() => select(active + 1), Math.max(1, interval));
    return () => window.clearInterval(timer);
  });
</script>

<div
  {...rest}
  data-carousel
  role="region"
  aria-roledescription="carousel"
  aria-label={label}
  class={styles.root({ class: className })}
  onfocusin={pauseForInteraction}
  onmouseenter={pauseForInteraction}
>
  <div
    data-carousel-viewport
    aria-live={isPlaying ? "off" : "polite"}
    class={styles.viewport({ class: viewportClass })}
  >
    {#if viewport}
      {@render viewport({slides, active, playing: isPlaying, firstIndex})}
    {:else}
    {#each slides as slide, index (slide)}
      <div
        data-carousel-slide
        role="group"
        aria-roledescription="slide"
        aria-label={`${index + 1} of ${slides.length} — ${slide.label}`}
        hidden={index !== active}
        class={styles.slide({ class: slideClass })}
      >
        {#if slide.content}
          {@render slide.content()}
        {:else if slide.image}
          <Image {...slide.image} priority={priority && index === firstIndex} class="w-full" />
        {/if}
      </div>
    {/each}
    {/if}
  </div>

  <div data-carousel-controls class={styles.controls({ class: controlsClass })}>
    <Button
      variant="secondary"
      size="sm"
      aria-label="Previous slide"
      disabled={slides.length < 2}
      onclick={() => select(active - 1)}
    >Previous</Button>
    <span data-carousel-position aria-live="off" class={styles.position()}>
      {slides.length === 0 ? "0 of 0" : `${active + 1} of ${slides.length}`}
    </span>
    <Button
      variant="secondary"
      size="sm"
      aria-label="Next slide"
      disabled={slides.length < 2}
      onclick={() => select(active + 1)}
    >Next</Button>
    {#if autoplay}
      <Button
        data-carousel-rotation
        variant="secondary"
        size="sm"
        aria-label={isPlaying ? "Pause slide rotation" : "Resume slide rotation"}
        aria-pressed={isPlaying}
        class={styles.rotation()}
        disabled={slides.length < 2 || reducedMotion.current}
        onclick={() => (rotationRequested = !rotationRequested)}
      >{isPlaying ? "Pause" : "Resume"}</Button>
    {/if}
  </div>
</div>
