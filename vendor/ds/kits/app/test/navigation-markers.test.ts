import { describe, expect, it } from "vitest";
import SidebarNavigation from "../src/composites/SidebarNavigation.svelte";
import SidebarPanel from "../src/composites/SidebarPanel.svelte";
import SidebarRail from "../src/composites/SidebarRail.svelte";
import { render, rendered } from "./mount";

describe("sidebar notifications and presence", () => {
  it("retains an exact accessible count while compacting the rail badge and announcing presence", () => {
    const root = rendered(render(SidebarRail, { label: "Areas", currentId: "inbox", items: [{ id: "inbox", label: "Inbox", badge: 125, status: { label: "Connected", tone: "success" } }] }));
    const button = root.querySelector('[data-sidebar-rail-item="inbox"]')!;
    expect(button.getAttribute("aria-label")).toBe("IN, Inbox, 99+ (125 notifications), Connected");
    expect(button.querySelector("[data-badge]")?.textContent).toBe("99+");
    expect(button.querySelector("[data-status-label]")?.textContent).toBe("Connected");
    expect(button.getAttribute("tabindex")).toBe("0");
    expect(button.querySelectorAll("button,a")).toHaveLength(0);
  });
  it("adds counts and presence to native sidebar destinations without replacing their links", () => {
    const root = rendered(render(SidebarNavigation, { label: "Workspace", currentId: "inbox", items: [{ id: "inbox", label: "Inbox", href: "/inbox", badge: 7, badgeLabel: "7 unread messages", status: { label: "Online", tone: "success" } }] }));
    const link = root.querySelector('a[href="/inbox"]')!;
    expect(link.getAttribute("aria-label")).toBe("Inbox, 7 unread messages, Online");
    expect(link.getAttribute("aria-current")).toBe("page");
    expect(link.querySelector("[data-badge]")?.textContent).toBe("7");
    expect(link.querySelector("[data-status-label]")?.textContent).toBe("Online");
  });
  it("allows notification and connection status in the panel heading", () => {
    const root = rendered(render(SidebarPanel, { title: "Conversations", badge: 7, status: { label: "Connected", tone: "success" } }));
    expect(root.getAttribute("aria-label")).toBe("Conversations, 7 notifications, Connected");
    expect(root.querySelector("[data-badge]")?.textContent).toBe("7");
    expect(root.querySelector("h2")?.textContent).toBe("Conversations");
  });
});
