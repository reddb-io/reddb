<!-- A caller-owned hierarchical view following the WAI-ARIA tree keyboard pattern. -->
<script module lang="ts">
  export { buildPathTree } from "./tree";
  export type {
    PathTreeOptions,
    TreeNode,
    TreeReorder,
    TreeReorderPosition,
  } from "./tree";
</script>

<script lang="ts">
  import type { TreeViewContract } from "./core/contracts/treeview";
  import { onDestroy, tick, type Snippet } from "svelte";
  import { quietControl, warnDeprecated } from "@reddb-io/design-system/base";
  import { flattenTree as flatten, findTreeNode as findNode, treeContains as contains, treeDropPosition, type VisibleTreeNode as VisibleNode, type TreeNode, type TreeReorder, type TreeReorderPosition } from "./tree";

  interface Props extends TreeViewContract {
    /** Reports the expanded node ids after a node opens or closes. */
    onexpandedchange?: (expanded: readonly string[]) => void;
    /** @deprecated Renamed `onexpandedchange` (ADR 0026); removed next release. */
    onExpanded?: (expanded: readonly string[]) => void;
    /** Reports the selected node id after the selection moves. */
    onselectedchange?: (selected: string | null) => void;
    /** @deprecated Renamed `onselectedchange` (ADR 0026); removed next release. */
    onSelected?: (selected: string | null) => void;
    /** Receives a committed rename: the node and its new label. */
    onrename?: (node: TreeNode, label: string) => void;
    /** @deprecated Renamed `onrename` (ADR 0026); removed next release. */
    onRename?: (node: TreeNode, label: string) => void;
    /** Receives a dropped move. */
    onreorder?: (move: TreeReorder) => void;
    /** @deprecated Renamed `onreorder` (ADR 0026); removed next release. */
    onReorder?: (move: TreeReorder) => void;
    /** Caller-owned decoration before a node's label, such as a kind icon. */
    leading?: Snippet<[TreeNode]>;
    /** Caller-owned detail after a node's label, such as a count or type. */
    trailing?: Snippet<[TreeNode]>;
    /** Extra classes, merged onto the root `ul` that is the tree. */
    class?: string;
  }


  let {
    label,
    nodes,
    expanded: controlledExpanded,
    onexpandedchange,
    onExpanded,
    selected: controlledSelected,
    onselectedchange,
    onSelected,
    renamable = true,
    onrename,
    onRename,
    reorderable = true,
    onreorder,
    onReorder,
    leading,
    trailing,
    class: className = "",
  }: Props = $props();

  let tree: HTMLUListElement;
  let localExpanded = $state<readonly string[]>([]);
  let localSelected = $state<string | null>(null);
  let activeId = $state<string | null>(null);
  let renamingId = $state<string | null>(null);
  let renameDraft = $state("");
  let renameEditor = $state<HTMLInputElement>();
  let draggedId: string | null = null;
  let typeahead = "";
  let typeaheadTimer: ReturnType<typeof setTimeout> | undefined;

  const activeExpanded = $derived(controlledExpanded ?? localExpanded);
  const activeSelected = $derived(
    controlledSelected === undefined ? localSelected : controlledSelected,
  );
  const expandedIds = $derived(new Set(activeExpanded));
  const visible = $derived(flatten(nodes, expandedIds));

  $effect(() => {
    if (!visible.some((entry) => entry.node.id === activeId)) {
      activeId = visible[0]?.node.id ?? null;
    }
  });

  // Each deprecated camelCase name still fires beside its canonical callback
  // for one release and warns once in development (ADR 0026).
  $effect(() => {
    if (onExpanded) warnDeprecated("TreeView", "onExpanded", "onexpandedchange");
    if (onSelected) warnDeprecated("TreeView", "onSelected", "onselectedchange");
    if (onRename) warnDeprecated("TreeView", "onRename", "onrename");
    if (onReorder) warnDeprecated("TreeView", "onReorder", "onreorder");
  });

  onDestroy(() => {
    if (typeaheadTimer) clearTimeout(typeaheadTimer);
  });

  function updateExpanded(next: readonly string[]): void {
    if (controlledExpanded === undefined) localExpanded = next;
    onexpandedchange?.(next);
    onExpanded?.(next);
  }

  function setExpanded(id: string, open: boolean): void {
    const next = open
      ? [...activeExpanded.filter((candidate) => candidate !== id), id]
      : activeExpanded.filter((candidate) => candidate !== id);
    updateExpanded(next);
  }

  function select(id: string): void {
    if (controlledSelected === undefined) localSelected = id;
    onselectedchange?.(id);
    onSelected?.(id);
  }

  function canRename(node: TreeNode): boolean {
    if (!onrename && !onRename) return false;
    return typeof renamable === "function" ? renamable(node) : renamable;
  }

  function beginRename(node: TreeNode): void {
    if (!canRename(node)) return;
    activeId = node.id;
    renamingId = node.id;
    renameDraft = node.label;
    void tick().then(() => {
      renameEditor?.focus();
      renameEditor?.select();
    });
  }

  function finishRename(node: TreeNode, commit: boolean): void {
    if (renamingId !== node.id) return;
    const next = renameDraft.trim();
    renamingId = null;
    if (commit && next.length > 0 && next !== node.label) {
      onrename?.(node, next);
      onRename?.(node, next);
    }
    void tick().then(() => focus(node.id));
  }

  function handleRenameKeydown(event: KeyboardEvent, node: TreeNode): void {
    if (event.key === "Enter") {
      event.preventDefault();
      finishRename(node, true);
    } else if (event.key === "Escape") {
      event.preventDefault();
      finishRename(node, false);
    }
  }

  function canReorder(node: TreeNode): boolean {
    if (!onreorder && !onReorder) return false;
    return typeof reorderable === "function" ? reorderable(node) : reorderable;
  }

  function dragSource(event: DragEvent): string | null {
    return draggedId || event.dataTransfer?.getData("text/plain") || null;
  }

  function canDrop(sourceId: string | null, target: TreeNode): sourceId is string {
    if (!sourceId || sourceId === target.id || !canReorder(target)) return false;
    const source = findNode(nodes, sourceId);
    return Boolean(source && !contains(source, target.id));
  }

  function handleDragStart(event: DragEvent, node: TreeNode): void {
    if (!canReorder(node)) return;
    draggedId = node.id;
    event.dataTransfer?.setData("text/plain", node.id);
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  }

  function handleDragOver(event: DragEvent, node: TreeNode): void {
    if (!canDrop(dragSource(event), node)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
  }

  function dropPosition(event: DragEvent, node: TreeNode): TreeReorderPosition {
    const bounds = event.currentTarget instanceof HTMLElement
      ? event.currentTarget.getBoundingClientRect()
      : { top: 0, height: 0 };
    const offset = event.clientY - bounds.top;
    return treeDropPosition(node, offset, bounds.height);
  }

  function handleDrop(event: DragEvent, node: TreeNode): void {
    const sourceId = dragSource(event);
    if (!canDrop(sourceId, node)) return;
    event.preventDefault();
    const move = { sourceId, targetId: node.id, position: dropPosition(event, node) };
    onreorder?.(move);
    onReorder?.(move);
    draggedId = null;
  }

  function focus(id: string): void {
    activeId = id;
    for (const item of tree?.querySelectorAll<HTMLElement>("[data-tree-id]") ?? []) {
      if (item.dataset.treeId === id) {
        item.focus();
        break;
      }
    }
  }

  function move(entry: VisibleNode, offset: number): void {
    const index = visible.indexOf(entry);
    const destination = visible[Math.min(visible.length - 1, Math.max(0, index + offset))];
    if (destination) focus(destination.node.id);
  }

  function searchByLabel(key: string, entry: VisibleNode): void {
    if (typeaheadTimer) clearTimeout(typeaheadTimer);
    typeahead += key.toLocaleLowerCase();
    typeaheadTimer = setTimeout(() => (typeahead = ""), 500);

    const start = visible.indexOf(entry) + 1;
    const candidates = [...visible.slice(start), ...visible.slice(0, start)];
    const match = candidates.find((candidate) =>
      candidate.node.label.toLocaleLowerCase().startsWith(typeahead),
    );
    if (match) focus(match.node.id);
  }

  function handleKeydown(event: KeyboardEvent, entry: VisibleNode): void {
    if (event.target !== event.currentTarget) return;
    const { node, parentId } = entry;
    const hasChildren = node.children.length > 0;
    const isExpanded = expandedIds.has(node.id);

    if (event.key === "ArrowDown") move(entry, 1);
    else if (event.key === "ArrowUp") move(entry, -1);
    else if (event.key === "Home") focus(visible[0]!.node.id);
    else if (event.key === "End") focus(visible[visible.length - 1]!.node.id);
    else if (event.key === "ArrowRight") {
      if (hasChildren && !isExpanded) setExpanded(node.id, true);
      else if (hasChildren) focus(node.children[0]!.id);
      else return;
    } else if (event.key === "ArrowLeft") {
      if (hasChildren && isExpanded) setExpanded(node.id, false);
      else if (parentId) focus(parentId);
      else return;
    } else if (event.key === "Enter" || event.key === " ") {
      select(node.id);
    } else if (event.key === "F2" && canRename(node)) {
      beginRename(node);
    } else if (
      event.key.length === 1 &&
      !event.ctrlKey &&
      !event.altKey &&
      !event.metaKey &&
      event.key !== " "
    ) {
      searchByLabel(event.key, entry);
    } else return;

    event.preventDefault();
  }
</script>

<ul
  bind:this={tree}
  role="tree"
  aria-label={label}
  data-tree-view
  class={`m-0 grid list-none gap-[var(--reddb-spatial-gap-sm)] p-0 text-sm text-foreground ${className}`.trim()}
>
  {#each visible as entry (entry.node.id)}
    <li
      role="treeitem"
      aria-expanded={entry.node.children.length > 0 ? expandedIds.has(entry.node.id) : undefined}
      aria-selected={activeSelected === entry.node.id}
      aria-level={entry.level}
      aria-posinset={entry.position}
      aria-setsize={entry.setSize}
      tabindex={activeId === entry.node.id ? 0 : -1}
      data-tree-id={entry.node.id}
      data-tree-reorderable={canReorder(entry.node) ? "" : undefined}
      draggable={canReorder(entry.node)}
      class="cursor-default rounded-sm px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-gap-sm)] hover:bg-foreground/8 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus aria-selected:bg-foreground/10 aria-selected:font-medium"
      style:padding-inline-start={`calc(var(--reddb-spatial-inset-sm) + ${(entry.level - 1)} * var(--reddb-spatial-inset-md))`}
      onclick={(event) => {
        activeId = entry.node.id;
        select(entry.node.id);
        event.currentTarget.focus();
      }}
      onkeydown={(event) => handleKeydown(event, entry)}
      ondragstart={(event) => handleDragStart(event, entry.node)}
      ondragover={(event) => handleDragOver(event, entry.node)}
      ondrop={(event) => handleDrop(event, entry.node)}
      ondragend={() => (draggedId = null)}
    >
      <span class="flex min-w-0 items-center gap-[var(--reddb-spatial-gap-sm)]">
        {#if canReorder(entry.node)}
          <span
            data-tree-drag-handle
            aria-hidden="true"
            title={`Drag ${entry.node.label} to reorder`}
            class="cursor-grab text-ink-muted"
          >⋮⋮</span>
        {/if}
        <span aria-hidden="true" class="inline-block w-[1em] shrink-0 text-ink-muted">
          {entry.node.children.length > 0 ? (expandedIds.has(entry.node.id) ? "▾" : "▸") : ""}
        </span>
        {#if renamingId === entry.node.id}
          <input
            bind:this={renameEditor}
            data-tree-rename={entry.node.id}
            aria-label={`Edit name for ${entry.node.label}`}
            value={renameDraft}
            onclick={(event) => event.stopPropagation()}
            oninput={(event) => (renameDraft = event.currentTarget.value)}
            onkeydown={(event) => handleRenameKeydown(event, entry.node)}
            onblur={() => finishRename(entry.node, true)}
            class="min-w-0 flex-1 rounded-sm border border-muted bg-background px-[var(--reddb-spatial-inset-sm)] text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus"
          />
        {:else}
          {#if leading}
            <span class="flex shrink-0 items-center" data-tree-leading>{@render leading(entry.node)}</span>
          {/if}
          <span class="min-w-0 flex-1 truncate">{entry.node.label}</span>
          {#if trailing}
            <span class="flex shrink-0 items-center" data-tree-trailing>{@render trailing(entry.node)}</span>
          {/if}
          {#if canRename(entry.node)}
            <button
              type="button"
              aria-label={`Rename ${entry.node.label}`}
              data-tree-rename-action
              class={quietControl({ class: "rounded-sm px-[var(--reddb-spatial-gap-sm)]" })}
              onclick={(event) => {
                event.stopPropagation();
                beginRename(entry.node);
              }}
            >Rename</button>
          {/if}
        {/if}
      </span>
    </li>
  {/each}
</ul>
