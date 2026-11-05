<!-- Dependency-free XML inspection using the Data Kit TreeView interaction contract. -->
<script module lang="ts">
  export { buildXmlTree, dottedPath } from "./viewer-tree";
  export type { ViewerTreeNode } from "./viewer-tree";
</script>

<script lang="ts">
  import type { XmlTreeContract } from "./core/contracts/xmltree";
  import { warnDeprecated } from "@reddb-io/design-system/base";
  import TreeView from "./TreeView.svelte";
  import { buildXmlTree } from "./viewer-tree";

  interface Props extends XmlTreeContract {
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
    label = "XML tree",
    expanded,
    onexpandedchange,
    onExpanded,
    selected,
    onselectedchange,
    onPath,
    class: className = "",
  }: Props = $props();

  $effect(() => {
    if (onExpanded) warnDeprecated("XmlTree", "onExpanded", "onexpandedchange");
    if (onPath) warnDeprecated("XmlTree", "onPath", "onselectedchange");
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

  const nodes = $derived(buildXmlTree(value));
</script>

<div data-xml-tree class={className}>
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
