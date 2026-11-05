<!--
  Logo — the Base Kit's first component, and a Primitive: it imports no other
  Kit component.

  The Brand owns the Mark; the DS owns the Logo that places it (ADR 0004). So
  this component does exactly four things, and refuses rather than improvising
  at each of them:

    * it SELECTS a Mark, by layout and by the surface behind it — `on` names
      the local background, not the global Theme, and defaults from the active
      Theme when the caller does not say (logo.surface.ts);
    * it REFUSES, loudly and by name, when the pinned Assets release ships no
      such Mark (today: the icon on dark). It never falls back to the other
      surface's drawing and never recolors one — both are licence violations,
      so neither has a code path (logo.marks.ts);
    * it holds the CLEARSPACE — 25% of the symbol's height — inside its own
      box, so a caller laying the Logo out cannot place anything in it, and
      cannot ask for the Mark without it. Below the minimum size is an error,
      not a smaller Logo (logo.box.ts);
    * given an `href` it becomes the accessible home link: a real <a>, an
      aria-label saying where it goes, and a focus ring on the box — which
      already includes the clearspace, so the ring falls outside it.

  When `on` is omitted, one small observer follows root Color Scheme changes so
  a persistent Logo swaps Mark as the document changes contrast.
-->
<script lang="ts">
  import type { HTMLAnchorAttributes, HTMLAttributes } from "svelte/elements";
  import { DEFAULT_SYMBOL_PX, logoBox, px } from "./logo.box";
  import { selectMark, type LogoLayout, type LogoSurface } from "./logo.marks";
  import { activeSurface, observeActiveSurface } from "./logo.surface";
  import { logo, logoMark } from "./logo.variants";

  interface Common {
    /** How the Mark is laid out. Defaults to `horizontal`. */
    layout?: LogoLayout;
    /**
     * The surface behind the Mark — the LOCAL background, not the Theme. A
     * dark hero on a light marketing page is `on="dark"`. Defaults from the
     * active Color Scheme.
     */
    on?: LogoSurface;
    /**
     * The symbol's height in CSS pixels — the red glyph, which is the same
     * drawing in every layout, so one number means the same thing across
     * layouts. The rendered box is larger by the clearspace on every side.
     */
    size?: number;
    /**
     * When the browser should fetch the Mark. `eager` by default, because the
     * Logo's commonest home is a masthead above the fold, where lazy would
     * defer the one image the page is identified by. A footer Logo is the case
     * for `lazy` — and it is the caller's to make, because the component
     * cannot see where it was placed.
     */
    loading?: "eager" | "lazy";
    /**
     * How the Mark's fetch is prioritised against the rest of the page. Left
     * to the browser by default; `high` is for the masthead Logo that should
     * not queue behind content below it.
     */
    fetchpriority?: "high" | "low" | "auto";
    /** Extra classes, merged over the component's own. */
    class?: string;
  }

  /** Without an `href`: the pure Mark — a footer, a chatbot avatar. */
  interface MarkMode extends Common, Omit<HTMLAttributes<HTMLSpanElement>, "class"> {
    href?: never;
    label?: never;
  }

  /** With an `href`: the accessible home link, as a navbar wears it. */
  interface LinkMode extends Common, Omit<HTMLAnchorAttributes, "class"> {
    /** Where it goes. Its presence is what makes this an <a>. */
    href: string;
    /**
     * What the link says it does, for anyone who cannot see the Mark. A
     * drawing has no text to fall back on, so the label is not optional — it
     * only has a default.
     */
    label?: string;
  }

  type Props = MarkMode | LinkMode;

  const {
    layout = "horizontal",
    on,
    size = DEFAULT_SYMBOL_PX,
    loading = "eager",
    fetchpriority,
    href,
    label = "reddb.io home",
    class: className,
    ...rest
  }: Props = $props();

  // Both of these throw on a Logo that cannot be placed honestly: a Mark the
  // release does not ship, and a size below the minimum. Deliberately at
  // render, and deliberately not caught — a Logo missing from a page is a bug
  // somebody files, and a wrong one is not.
  let observedSurface = $state(activeSurface());
  let markImageElement: HTMLImageElement | undefined = $state();
  $effect(() => {
    if (on !== undefined) return;
    return observeActiveSurface((next) => {
      observedSurface = next;
    });
  });

  const surface = $derived(on ?? observedSurface);
  const mark = $derived(selectMark(layout, surface));
  const box = $derived(logoBox(mark, size));

  // An SSR render has no document and therefore begins on the light technical
  // fallback. Keep the opaque image URL synchronized explicitly when hydration
  // discovers that the real document is dark; metadata changing without the
  // decoded image changing would display the wrong licensed Mark.
  $effect(() => {
    if (markImageElement && markImageElement.src !== mark.src) {
      markImageElement.src = mark.src;
    }
  });

  const tag = $derived(href !== undefined ? "a" : "span");
</script>

<!--
  `rest` is spread as a plain record for the reason Button's is: its type is
  the remainder of a two-armed union of platform props, and re-deriving that
  against what <span> and <a> each accept is a computation TypeScript gives up
  on. Callers are still typed where they write, at `Props`.

  The clearspace is padding on this box, so the Mark's own size and the box's
  size stay separately true: the <img> is the drawing, the padding is the space
  the licence keeps around it, and there is no prop that removes it.
-->
<svelte:element
  this={tag}
  {...rest as Record<string, unknown>}
  {href}
  class={logo({ interactive: href !== undefined, class: className })}
  style="padding:{px(box.clearspace)}"
  aria-label={href !== undefined ? label : undefined}
  data-logo-layout={layout}
  data-logo-on={surface}
  data-logo-mark={mark.file}
>
  <!--
    In link mode the <a> already says where it goes, so the Mark is decorative
    and an alt would say the same thing twice. On its own the Mark IS the
    content, and carries the Brand's name.

    The fetch hints below are named props rather than part of `rest`, and that
    is the whole reason they exist here: `rest` is spread on the BOX, so an
    image attribute travelling in it would land on a <span> or an <a>, which
    have no use for one — dropped without a word, leaving the caller believing
    it took. `decoding` is fixed instead of a prop because no Logo's
    correctness depends on the decode landing in the same frame as the markup
    around it, which is all `sync` buys; `loading` and `fetchpriority` are the
    caller's, because their right answer is the one fact this component cannot
    know — where on the page it was placed.
  -->
  {#key mark.file}
    <!-- The key replaces the opaque image node when the active surface changes.
         The Brand bytes stay untouched; the browser receives the newly selected
         asset rather than retaining the previously decoded data URL. -->
    <img
      bind:this={markImageElement}
      src={mark.src}
      alt={href !== undefined ? "" : "RedDB"}
      width={Math.round(box.markWidth)}
      height={Math.round(box.markHeight)}
      style="width:{px(box.markWidth)};height:{px(box.markHeight)}"
      class={logoMark()}
      {loading}
      {fetchpriority}
      decoding="async"
      aria-hidden={href !== undefined ? "true" : undefined}
    />
  {/key}
</svelte:element>
