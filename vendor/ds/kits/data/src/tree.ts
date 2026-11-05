/** Pure path operations for the Data Kit TreeView. This module has no Svelte or DOM dependency. */

export interface TreeNode<Value = unknown> {
  /** Stable node identity. Path-built nodes use their normalized full path. */
  id: string;
  /** Visible node name. */
  label: string;
  /** Caller-owned value when this node represents an input path. */
  value?: Value;
  children: readonly TreeNode<Value>[];
}

export interface PathTreeOptions {
  /** Path separator. Repeated and leading/trailing separators are ignored. */
  separator?: string;
}

export type TreeReorderPosition = "before" | "after" | "inside";

/** A caller-owned move proposed by the TreeView's drag-and-drop affordance. */
export interface TreeReorder {
  sourceId: string;
  targetId: string;
  position: TreeReorderPosition;
}

interface MutableTreeNode<Value> {
  id: string;
  label: string;
  value?: Value;
  children: Map<string, MutableTreeNode<Value>>;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function materialize<Value>(nodes: Iterable<MutableTreeNode<Value>>): readonly TreeNode<Value>[] {
  return [...nodes]
    .sort((left, right) => collator.compare(left.label, right.label))
    .map((node) => {
      const result: TreeNode<Value> = {
        id: node.id,
        label: node.label,
        children: materialize(node.children.values()),
      };
      if ("value" in node) result.value = node.value;
      return result;
    });
}

/**
 * Turn a path-keyed record into an immutable tree. The record and its values remain caller-owned;
 * only the node structure is created here.
 */
export function buildPathTree<Value>(
  paths: Readonly<Record<string, Value>>,
  options: PathTreeOptions = {},
): readonly TreeNode<Value>[] {
  const separator = options.separator ?? "/";
  if (separator.length === 0) throw new RangeError("The path separator cannot be empty.");

  const roots = new Map<string, MutableTreeNode<Value>>();
  for (const [path, value] of Object.entries(paths)) {
    const segments = path.split(separator).filter((segment) => segment.length > 0);
    let siblings = roots;
    const ancestors: string[] = [];

    for (const [index, label] of segments.entries()) {
      ancestors.push(label);
      let node = siblings.get(label);
      if (!node) {
        node = { id: ancestors.join(separator), label, children: new Map() };
        siblings.set(label, node);
      }
      if (index === segments.length - 1) node.value = value;
      siblings = node.children;
    }
  }

  return materialize(roots.values());
}

/** One visible row in the flattened WAI-ARIA tree, retaining sibling relationships. */
export interface VisibleTreeNode {node:TreeNode;level:number;parentId:string|null;position:number;setSize:number;}
/** Visible reading order, shared by native tree adapters. */
export function flattenTree(siblings:readonly TreeNode[],open:ReadonlySet<string>,level=1,parentId:string|null=null):readonly VisibleTreeNode[]{
 const result:VisibleTreeNode[]=[];for(const[index,node]of siblings.entries()){result.push({node,level,parentId,position:index+1,setSize:siblings.length});if(node.children.length&&open.has(node.id))result.push(...flattenTree(node.children,open,level+1,node.id));}return result;
}
/** Resolve a caller-owned identity without depending on a DOM selector's escaping rules. */
export function findTreeNode(siblings:readonly TreeNode[],id:string):TreeNode|undefined {for(const node of siblings){if(node.id===id)return node;const child=findTreeNode(node.children,id);if(child)return child;}return undefined;}
/** Reject recursive reorder targets before asking a caller to mutate its model. */
export function treeContains(node:TreeNode,id:string):boolean{return node.children.some(child=>child.id===id||treeContains(child,id));}
/** Divide a measured row into before/inside/after drop zones. */
export function treeDropPosition(node:TreeNode,offset:number,height:number):TreeReorderPosition{return node.children.length&&offset>=height/3&&offset<=height*2/3?"inside":offset<height/2?"before":"after";}
