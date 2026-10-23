import {
  ActionPanel,
  applicationShell,
  commandPalette,
  contextMenu,
  pageHeading,
  sidebarLayout,
  sidebarNavigation,
  sidebarRail,
} from "@reddb-io/design-system/app";
import { popover } from "@reddb-io/design-system/base";
import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { classes, classesOf, render, rendered } from "./mount";

const chromeContract = [
  "bg-elevation-sunken-surface",
  "border-elevation-sunken-border",
  "shadow-elevation-sunken",
] as const;

describe("Application elevation", () => {
  it("keeps content at base while only persistent shell chrome uses the complete sunken material", () => {
    const shell = applicationShell();
    expect(classesOf(shell.root()).has("bg-elevation-base-surface")).toBe(true);
    expect(classesOf(shell.main()).has("bg-elevation-base-surface")).toBe(true);

    const chrome = {
      ShellHeader: shell.header(),
      ShellFooter: shell.footer(),
      SidebarRail: sidebarRail().root(),
      SidebarPanel: sidebarLayout({ rail: true }).panel(),
      SidebarNavigation: sidebarNavigation().root(),
    };

    for (const [name, surface] of Object.entries(chrome)) {
      const worn = classesOf(surface);
      for (const token of chromeContract) {
        expect(worn.has(token), `${name} does not resolve ${token}`).toBe(true);
      }
    }

    const heading = classesOf(pageHeading().root());
    expect(heading.has("bg-transparent")).toBe(true);
    expect(heading.has("bg-elevation-sunken-surface")).toBe(false);
    expect(heading.has("border-elevation-sunken-border")).toBe(true);
    expect(heading.has("shadow-elevation-sunken")).toBe(false);
  });

  it("keeps ContextMenu and CommandPalette on the inherited overlay material", () => {
    const surfaces = {
      ContextMenu: contextMenu().content(),
      CommandPalette: popover({ class: commandPalette().content() }),
    };

    for (const [name, surface] of Object.entries(surfaces)) {
      const worn = classesOf(surface);
      for (const token of [
        "bg-elevation-overlay-surface",
        "border-elevation-overlay-border",
        "shadow-elevation-overlay",
      ]) {
        expect(worn.has(token), `${name} does not resolve ${token}`).toBe(true);
      }
    }
  });

  it("raises application panels through the Card contract", () => {
    const panel = rendered(render(ActionPanel, {
      title: "Deployment actions",
      actions: createRawSnippet(() => ({ render: () => "<button>Deploy</button>" })),
    }));

    expect(panel.dataset.raised).toBe("true");
    expect(classes(panel).has("shadow-elevation-raised")).toBe(true);
  });
});
