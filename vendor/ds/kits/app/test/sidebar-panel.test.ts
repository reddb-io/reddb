import { createRawSnippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import SidebarPanel from "../src/composites/SidebarPanel.svelte";
import { render, rendered } from "./mount";

describe("contextual sidebar panel", () => {
  it("offers only requested actions and a named scrolling body", () => {
    const ondismiss = vi.fn();
    const root = rendered(render(SidebarPanel, { title: "Workspace", backHref: "/workspaces", backLabel: "All workspaces", ondismiss }));
    expect(root.querySelector('a[href="/workspaces"]')?.textContent).toContain("All workspaces");
    root.querySelector<HTMLButtonElement>('button[aria-label="Close Workspace"]')!.click();
    expect(ondismiss).toHaveBeenCalledTimes(1);
    const body = root.querySelector('[data-scroll-area]');
    expect(body?.getAttribute("aria-label")).toBe("Workspace items");
    expect(body?.getAttribute("tabindex")).toBe("0");
  });
  it("replaces both default contextual controls when a toolbar is provided", () => {
    const toolbar = createRawSnippet(() => ({ render: () => '<a href="/custom">Custom context</a>' }));
    const root = rendered(render(SidebarPanel, { title: "Workspace", backHref: "/workspaces", ondismiss: vi.fn(), toolbar }));
    const row = root.querySelector('[data-sidebar-panel-toolbar]')!;
    expect(row.querySelectorAll("a,button")).toHaveLength(1);
    expect(row.querySelector('a')?.getAttribute("href")).toBe("/custom");
    expect(row.querySelector("button")).toBeNull();
  });
  it("adds no automatic contextual controls to a content-only panel", () => {
    const root = rendered(render(SidebarPanel, { title: "Workspace" }));
    expect(root.querySelector('[data-sidebar-panel-toolbar]')).toBeNull();
  });
});
