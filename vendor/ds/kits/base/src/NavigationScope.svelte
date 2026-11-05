<!--
  NavigationScope — the injectable navigation seam (issue 483).

  Every in-app link inside it, whichever component rendered it, is activated
  through `navigate` instead of being followed by the browser. It renders no
  box of its own (`display: contents`), so it never changes layout. Without a
  scope, links stay native anchors.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { ownsSameOrigin, routableAnchor, setNavigation } from "./navigation";

  import type { NavigationSeam } from "./core/navigation";

  interface Props extends NavigationSeam {
    /**
     * Content whose in-app links the scope routes through `navigate`; the scope adds no box of
     * its own.
     */
    children?: Snippet;
  }

  const { navigate, resolve, owns, children }: Props = $props();
  let scope = $state<HTMLElement>();
  // Interception needs the client: until hydration the browser follows links
  // natively. The attribute says when the seam is live (tests, diagnostics).
  let active = $state(false);
  $effect(() => {
    active = true;
  });

  setNavigation({
    navigate: (href, event) => navigate(href, event),
    resolve: (to) => (resolve ? resolve(to) : to),
    owns: (href, anchor) => (owns ?? ownsSameOrigin)(href, anchor),
  });

  function activate(event: MouseEvent): void {
    if (!scope) return;
    const anchor = routableAnchor(event, scope, owns ?? ownsSameOrigin);
    if (!anchor) return;
    event.preventDefault();
    navigate(anchor.getAttribute("href")!, event);
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<!-- Delegation only: the anchors keep their own keyboard activation, which the browser reports as this same click. -->
<div
  bind:this={scope}
  data-navigation-scope
  data-navigation-active={active ? "" : undefined}
  class="contents"
  onclick={activate}
>
  {@render children?.()}
</div>
