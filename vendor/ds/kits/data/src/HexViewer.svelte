<!-- Dependency-free hexadecimal inspection with fixed-width text columns. -->
<script module lang="ts">
  export { formatHex, hexBytes } from "./hex";
  export type { HexInput, HexRow } from "./hex";
</script>

<script lang="ts">
  import type { HexViewerContract } from "./core/contracts/hexviewer";
  import { formatHex, type HexInput } from "./hex";

  interface Props extends HexViewerContract {
    /** Extra classes, merged onto the root `section`. */
    class?: string;
  }

  let {
    value,
    bytesPerRow = 16,
    label = "Hex viewer",
    class: className = "",
  }: Props = $props();

  const rows = $derived(formatHex(value, bytesPerRow));
  const offsetHeader = $derived("Offset".padEnd(rows[0]?.offset.length ?? 8, " "));
  const hexHeader = $derived("Hex".padEnd(bytesPerRow * 3 - 1, " "));
  const asciiHeader = $derived("ASCII".padEnd(bytesPerRow, " "));
</script>

<!--
  A <div>, not a <section>: a section is a landmark and ARIA in HTML does not
  let it become a table (wave 5A). The grid stays an ARIA table so offsets,
  bytes and ASCII are read as the three columns they are.
-->
<div
  data-hex-viewer
  role="table"
  aria-label={label}
  aria-colcount="3"
  aria-rowcount={rows.length + 1}
  class={`w-full overflow-x-auto rounded-lg border border-muted bg-transparent text-foreground ${className}`.trim()}
>
  <div class="min-w-max font-mono text-sm leading-relaxed">
    <div
      role="row"
      class="grid grid-cols-[max-content_max-content_max-content] gap-[var(--reddb-spatial-gap-lg)] border-b border-muted px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] font-medium whitespace-pre text-ink-muted"
    >
      <span role="columnheader">{offsetHeader}</span>
      <span role="columnheader">{hexHeader}</span>
      <span role="columnheader">{asciiHeader}</span>
    </div>

    <div role="rowgroup" class="py-[var(--reddb-spatial-inset-sm)]">
      {#each rows as row (row.offset)}
        <div
          data-hex-row
          role="row"
          class="grid grid-cols-[max-content_max-content_max-content] gap-[var(--reddb-spatial-gap-lg)] px-[var(--reddb-spatial-inset-md)] whitespace-pre hover:bg-foreground/8"
        >
          <span data-hex-offset role="cell" class="text-ink-muted">{row.offset}</span>
          <span data-hex-bytes role="cell">{row.hex}</span>
          <span data-hex-ascii role="cell" class="text-ink-muted">{row.ascii}</span>
        </div>
      {:else}
        <div role="row" class="px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] text-ink-muted">
          <span role="cell" aria-colspan="3">No bytes.</span>
        </div>
      {/each}
    </div>
  </div>
</div>
