<!-- Dependency-free JSON inspection using the Data Kit TreeView interaction contract. -->
<script module lang="ts">
  export { buildJsonTree, dottedPath } from "./viewer-tree";
  export type { ViewerTreeNode } from "./viewer-tree";
</script>

<script lang="ts">
  import type { JsonTreeContract } from "./core/contracts/jsontree";
  import { warnDeprecated } from "@reddb-io/design-system/base";
  import TreeView from "./TreeView.svelte";
  import { buildJsonTree } from "./viewer-tree";

  interface Props extends JsonTreeContract {
    /** Reports the expanded paths after a node opens or closes. */
    onexpandedchange?: (expanded: readonly string[]) => void;
    /** @deprecated Renamed `onexpandedchange` (ADR 0026); removed next release. */
    onExpanded?: (expanded: readonly string[]) => void;
    /** Receives the exact dotted path stored on the newly selected node. */
    onselectedchange?: (path: string) => void;
    /** @deprecated Renamed `onselectedchange` (ADR 0026); removed next release. */
    onPath?: (path: string) => void;
    /** Extra classes, merged onto the wrapper `div` around the tree. */
    class?: string;
  }

  let {
    value,
    label = "JSON tree",
    expanded,
    onexpandedchange,
    onExpanded,
    selected,
    onselectedchange,
    onPath,
    class: className = "",
  }: Props = $props();

  $effect(() => {
    if (onExpanded) warnDeprecated("JsonTree", "onExpanded", "onexpandedchange");
    if (onPath) warnDeprecated("JsonTree", "onPath", "onselectedchange");
  });

  function reportExpanded(next: readonly string[]): void {
    onexpandedchange?.(next);
    onExpanded?.(next);
  }

  function reportSelected(path: string | null): void {
    if (path === null) return;
    onselectedchange?.(path);
    onPath?.(path);
  }

  const nodes = $derived(buildJsonTree(value));
</script>

<div data-json-tree class={className}>
  <TreeView
    {label}
    {nodes}
    {expanded}
    onexpandedchange={reportExpanded}
    {selected}
    onselectedchange={reportSelected}
    renamable={false}
    reorderable={false}
  />
</div>
