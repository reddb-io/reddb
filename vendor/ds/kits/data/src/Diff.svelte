<!-- Dependency-free field/line comparison with textual change cues and an explicit summary. -->
<script module lang="ts">
  export { diffFields, diffLines, formatDiffValue } from "./diff";
  export type { DiffEntry, DiffKind, DiffResult, DiffSummary } from "./diff";
</script>

<script lang="ts">
  import type { DiffContract } from "./core/contracts/diff";
  import { diffFields, diffLines, formatDiffValue, type DiffKind } from "./diff";

  interface Props extends DiffContract {
    /** Extra classes, merged onto the root `section`. */
    class?: string;
  }

  let {
    before,
    after,
    mode = "fields",
    label = "Changes",
    showUnchanged = true,
    class: className = "",
  }: Props = $props();

  const result = $derived(
    mode === "lines" ? diffLines(String(before ?? ""), String(after ?? "")) : diffFields(before, after),
  );
  const entries = $derived(
    showUnchanged ? result.entries : result.entries.filter((entry) => entry.kind !== "unchanged"),
  );

  const markers: Record<DiffKind, string> = {
    added: "+",
    removed: "−",
    changed: "~",
    unchanged: " ",
  };

  function lineClasses(kind: DiffKind): string {
    if (kind === "added") {
      return "bg-[var(--reddb-color-feedback-success-surface)] text-[var(--reddb-color-feedback-success-foreground)]";
    }
    if (kind === "removed") {
      return "bg-[var(--reddb-color-feedback-danger-surface)] text-[var(--reddb-color-feedback-danger-foreground)]";
    }
    if (kind === "changed") {
      return "bg-[var(--reddb-color-feedback-warning-surface)] text-[var(--reddb-color-feedback-warning-foreground)]";
    }
    return "bg-transparent text-foreground";
  }
</script>

<section
  data-diff-viewer
  data-mode={mode}
  aria-label={label}
  class={`w-full min-w-0 overflow-hidden rounded-lg border border-muted bg-transparent text-foreground ${className}`.trim()}
>
  <p
    data-diff-summary
    aria-live="polite"
    class="m-0 border-b border-muted px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] text-sm font-medium"
  >
    {result.summary.added} added · {result.summary.removed} removed · {result.summary.changed} changed · {result.summary.unchanged} unchanged
  </p>

  <div role="list" class="m-0 overflow-x-auto py-[var(--reddb-spatial-inset-sm)] font-mono text-sm leading-relaxed">
    {#each entries as entry, index (`${entry.path}:${entry.kind}:${index}`)}
      <div
        data-diff-entry
        data-change-kind={entry.kind}
        role="listitem"
        aria-label={`${entry.kind}: ${entry.path}`}
        class={`grid grid-cols-[auto_minmax(0,1fr)] gap-[var(--reddb-spatial-gap-sm)] px-[var(--reddb-spatial-inset-md)] whitespace-pre ${lineClasses(entry.kind)}`}
      >
        <span data-diff-marker aria-hidden="true" class="select-none">{markers[entry.kind]}</span>
        <span class="min-w-0">
          <strong>{entry.path}</strong>
          {#if entry.kind === "changed"}
            <span>: <del>{formatDiffValue(entry.before)}</del> → <ins>{formatDiffValue(entry.after)}</ins></span>
          {:else if entry.kind === "removed"}
            <span>: {formatDiffValue(entry.before)}</span>
          {:else}
            <span>: {formatDiffValue(entry.after)}</span>
          {/if}
        </span>
      </div>
    {/each}
  </div>
</section>
