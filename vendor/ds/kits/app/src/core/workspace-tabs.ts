export interface WorkspaceTab {
  /** Stable application-owned identity. */
  id: string;
  /** Visible tab name. */
  label: string;
  /** Whether the application accepts close requests. */
  closable?: boolean;
  /** Caller-observed unsaved changes, independent of execution. */
  dirty?: boolean;
  /** Caller-observed operation in progress. */
  busy?: boolean;
  /** Prevent selection and closing. */
  disabled?: boolean;
}
export function workspaceTabs(
  items: readonly WorkspaceTab[],
): readonly WorkspaceTab[] {
  const ids = new Set<string>();
  for (const item of items) {
    if (!item.id.trim() || ids.has(item.id))
      throw new Error("WorkspaceTabs requires unique non-empty ids.");
    ids.add(item.id);
  }
  return items;
}
export function workspaceSelection(
  items: readonly WorkspaceTab[],
  selected?: string | null,
): string {
  return (
    items.find((item) => item.id === selected && !item.disabled)?.id ??
    items.find((item) => !item.disabled)?.id ??
    ""
  );
}
/** Restore focus only when removing the workspace control that owned it. */
export function mountWorkspaceFocus(root: HTMLElement) {
  let focused: string | null = null;
  const remember = (event: FocusEvent) => {
    focused =
      (event.target as HTMLElement).closest<HTMLElement>(
        "[data-workspace-item]",
      )?.dataset.workspaceItem ?? null;
  };
  root.addEventListener("focusin", remember);
  return {
    sync(items: readonly WorkspaceTab[], selected: string) {
      if (
        !focused ||
        items.some((item) => item.id === focused && !item.disabled)
      )
        return;
      focused = null;
      const active = root.ownerDocument.activeElement;
      if (
        !active ||
        active === root.ownerDocument.body ||
        root.contains(active)
      ) {
        [...root.querySelectorAll<HTMLButtonElement>('[role="tab"]')]
          .find((item) => item.dataset.workspaceTab === selected)
          ?.focus();
      }
    },
    destroy() {
      root.removeEventListener("focusin", remember);
    },
  };
}
