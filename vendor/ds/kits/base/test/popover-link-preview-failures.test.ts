import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import PopoverLinkPreviewFailures from "./fixtures/PopoverLinkPreviewFailures.svelte";
import { render } from "./mount";

function viewportFailures(root: HTMLElement, viewportWidth: number): string[] {
  const overlay = root.querySelector<HTMLElement>("[data-broken-overlay]")!;
  const left = Number.parseFloat(overlay.style.left);
  const width = Number.parseFloat(overlay.style.width);
  return left + width <= viewportWidth ? [] : ["overlay escapes the viewport"];
}

describe("the deliberately failing Popover and LinkPreview fixtures", () => {
  it("diagnoses an anchored overlay that escapes the viewport", () => {
    const root = render(PopoverLinkPreviewFailures, { failure: "viewport-escape" });
    expect(viewportFailures(root, 1024)).toEqual(["overlay escapes the viewport"]);
  });

  it("diagnoses an overlay that traps focus permanently", () => {
    const root = render(PopoverLinkPreviewFailures, { failure: "permanent-focus-trap" });
    root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!.click();
    flushSync();

    const overlay = root.querySelector<HTMLElement>("[data-broken-overlay]")!;
    const trapped = overlay.querySelector<HTMLButtonElement>("button")!;
    trapped.focus();

    const tab = new KeyboardEvent("keydown", {
      key: "Tab",
      bubbles: true,
      cancelable: true,
    });
    trapped.dispatchEvent(tab);
    overlay.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    flushSync();

    expect(tab.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(trapped);
    expect(root.querySelector("[data-broken-overlay]") ? ["focus trap cannot be dismissed"] : []).toEqual([
      "focus trap cannot be dismissed",
    ]);
  });
});
