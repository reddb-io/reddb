// Who owns `<main>` (wave 5A, DESIGN.md "Landmarks"): ApplicationShell does.
// A SidebarLayout or a second ApplicationShell inside it renders a plain
// region, so composing them never yields a duplicate or a nested `main`.
// SidebarLayout's `main` and ApplicationShell's `embedded` override the default.

import { describe, expect, it } from "vitest";
import MainOwnershipConsumer from "./fixtures/MainOwnershipConsumer.svelte";
import { render } from "./mount";

describe("main landmark ownership", () => {
  it("gives the one <main> to the ApplicationShell around a SidebarLayout", () => {
    const root = render(MainOwnershipConsumer, { case: "sidebar-in-shell" });
    const mains = root.querySelectorAll("main");
    const region = root.querySelector<HTMLElement>('[data-sidebar-layout-region="main"]')!;

    expect(mains).toHaveLength(1);
    expect(mains[0]!.matches('[data-application-shell-region="main"]')).toBe(true);
    expect(region.tagName).toBe("DIV");
    // No second "main-content": the region takes no id unless given one.
    expect(region.hasAttribute("id")).toBe(false);
    expect(root.querySelectorAll("#main-content")).toHaveLength(1);
  });

  it("lets SidebarLayout take <main> explicitly when the shell yields it", () => {
    const root = render(MainOwnershipConsumer, { case: "sidebar-forced-main" });
    const mains = root.querySelectorAll("main");

    expect(mains).toHaveLength(1);
    expect(mains[0]!.matches('[data-sidebar-layout-region="main"]')).toBe(true);
    expect(mains[0]!.id).toBe("workspace");
  });

  it("lets an embedded shell's SidebarLayout render the one <main>: the shell claims nothing", () => {
    const root = render(MainOwnershipConsumer, { case: "sidebar-in-embedded-shell" });
    const mains = root.querySelectorAll("main");

    expect(mains).toHaveLength(1);
    expect(mains[0]!.matches('[data-sidebar-layout-region="main"]')).toBe(true);
  });

  it("embeds an ApplicationShell nested in another one", () => {
    const root = render(MainOwnershipConsumer, { case: "shell-in-shell" });

    expect(root.querySelectorAll("main")).toHaveLength(1);
    expect(root.querySelector("#preview")?.tagName).toBe("DIV");
  });

  it("keeps a standalone SidebarLayout's <main>, and honours main={false}", () => {
    const alone = render(MainOwnershipConsumer, { case: "sidebar-alone" });
    expect(alone.querySelector("main")?.id).toBe("main-content");

    const optOut = render(MainOwnershipConsumer, { case: "sidebar-opt-out" });
    expect(optOut.querySelector("main")).toBeNull();
    expect(optOut.querySelector('[data-sidebar-layout-region="main"]')?.tagName).toBe("DIV");
  });
});
