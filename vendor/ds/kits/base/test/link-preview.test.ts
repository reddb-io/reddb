import { createRawSnippet, flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import { LinkPreview, linkPreview as linkPreviewAppearance } from "@reddb-io/design-system/base";
import { classes, classesOf, render } from "./mount";

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

function surface(): HTMLElement | null {
  return document.querySelector("[data-link-preview-surface]");
}

describe("the Base LinkPreview", () => {
  it("opens from keyboard focus without replacing the native link", async () => {
    const root = render(LinkPreview, {
      href: "/clusters/primary",
      label: "Primary cluster",
      previewLabel: "Primary cluster preview",
      openDelay: 0,
      children: createRawSnippet(() => ({
        render: () => '<button type="button" data-preview-action>Open cluster</button>',
      })),
    });
    const trigger = root.querySelector<HTMLAnchorElement>("[data-link-preview-trigger]")!;

    expect(trigger.tagName).toBe("A");
    expect(trigger.getAttribute("href")).toBe("/clusters/primary");
    expect(trigger.getAttribute("role")).toBe("link");
    trigger.focus();
    await settle();

    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(surface()?.getAttribute("role")).toBe("dialog");
    expect(surface()?.getAttribute("aria-label")).toBe("Primary cluster preview");
    expect(document.activeElement).toBe(trigger);
    expect(surface()?.tabIndex).toBe(-1);
  });

  it("dismisses with Escape without trapping or moving focus", async () => {
    const root = render(LinkPreview, {
      href: "/regions",
      label: "Regions",
      previewLabel: "Regions preview",
      open: true,
      openDelay: 0,
      closeDelay: 0,
    });
    const trigger = root.querySelector<HTMLAnchorElement>("[data-link-preview-trigger]")!;
    trigger.focus();
    await settle();
    expect(surface()).not.toBeNull();

    trigger.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();

    expect(surface()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("uses the shared collision-aware surface appearance", async () => {
    render(LinkPreview, {
      href: "/metrics",
      label: "Metrics",
      previewLabel: "Metrics preview",
      open: true,
      collisionPadding: 16,
      class: "max-w-lg",
    });
    await settle();
    const element = surface()!;

    expect(element.dataset.avoidCollisions).toBe("true");
    expect(element.dataset.collisionPadding).toBe("16");
    expect(classes(element)).toEqual(classesOf(linkPreviewAppearance({ class: "max-w-lg" })));
    expect(classes(element).has("motion-reduce:transition-none")).toBe(true);
  });
});
