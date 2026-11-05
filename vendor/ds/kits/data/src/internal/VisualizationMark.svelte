<script lang="ts">
  import type { Snippet } from "svelte";
  let { id, label, selected, onselect, children }: { id: string; label: string; selected?: string; onselect?: (id: string) => void; children: Snippet } = $props();
</script>
<!-- svelte-ignore a11y_no_noninteractive_tabindex (Keyboard access to the scroll region or conditionally interactive mark.) -->
<g data-mark-id={id} data-selected={selected === id ? "" : undefined}
  role={onselect ? "button" : "group"} tabindex={onselect ? 0 : undefined}
  aria-pressed={onselect ? selected === id : undefined}
  onclick={onselect ? () => onselect?.(id) : undefined}
  onkeydown={onselect ? (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); onselect?.(id); } } : undefined}
  class="outline-offset-2 focus-visible:outline-2 focus-visible:outline-focus"
  style:cursor={onselect ? "pointer" : undefined}>
  <title>{label}</title>
  {@render children()}
</g>
