import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import Diff, { type DiffRow } from "../src/composites/Diff.svelte";
import { diff } from "../src/composites/diff.variants";
import DiffContractFailures from "./fixtures/DiffContractFailures.svelte";
import DiffConsumer from "./fixtures/DiffConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const ROWS: readonly DiffRow[] = [
  { before: "[server]", after: "[server]" },
  { before: "timeout = 30", after: "timeout = 60" },
  { before: "legacy = true" },
  { after: "retries = 3" },
];

describe("the deliberately failing Diff fixture", () => {
  it("demonstrates additions and removals conveyed by colour alone", () => {
    const broken = rendered(render(DiffContractFailures));

    expect(broken.querySelectorAll(".text-danger, .text-success")).toHaveLength(2);
    expect(broken.querySelector("[data-change-kind]")).toBeNull();
    expect(broken.textContent).not.toMatch(/Added|Removed|[+−]/);
  });
});

describe("Diff", () => {
  it("presents an inline comparison with visible and announced change kinds", () => {
    const diff = rendered(render(Diff, { label: "Configuration changes", rows: ROWS }));
    const lines = [...diff.querySelectorAll<HTMLElement>("[data-diff-line]")];

    expect(diff.matches('[data-diff][data-mode="inline"][aria-label="Configuration changes"]'))
      .toBe(true);
    expect(lines.map((line) => line.dataset.changeKind)).toEqual([
      "context",
      "removal",
      "addition",
      "removal",
      "addition",
    ]);
    expect(lines.map((line) => line.getAttribute("aria-label"))).toEqual([
      null,
      "Removed: timeout = 30",
      "Added: timeout = 60",
      "Removed: legacy = true",
      "Added: retries = 3",
    ]);
    expect(lines.map((line) => line.querySelector("[data-diff-marker]")?.textContent)).toEqual([
      "",
      "−",
      "+",
      "−",
      "+",
    ]);
  });

  it("composes the canonical keyboard-resizable SplitView for side-by-side changes", () => {
    const diff = rendered(
      render(Diff, {
        label: "Configuration changes",
        rows: ROWS,
        mode: "side-by-side",
        beforeLabel: "Current",
        afterLabel: "Proposed",
        separatorLabel: "Resize current and proposed configuration",
      }),
    );
    const split = diff.querySelector<HTMLElement>('[data-orientation="horizontal"]')!;
    const panes = [...diff.querySelectorAll<HTMLElement>("[data-diff-pane]")];
    const separator = split.querySelector<HTMLElement>('[role="separator"]')!;

    expect(diff.dataset.mode).toBe("side-by-side");
    expect(panes.map((pane) => pane.getAttribute("aria-label"))).toEqual([
      "Current",
      "Proposed",
    ]);
    expect(panes.map((pane) => pane.querySelectorAll("[data-diff-line]").length)).toEqual([4, 4]);
    expect(separator.getAttribute("aria-label")).toBe(
      "Resize current and proposed configuration",
    );

    separator.focus();
    separator.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }),
    );
    flushSync();
    expect(document.activeElement).toBe(separator);
    expect(separator.getAttribute("aria-valuenow")).toBe("52");
  });

  it("keeps Density tokenized and inherits every nested appearance axis", () => {
    const scope = rendered(render(DiffConsumer));
    const comparison = scope.querySelector<HTMLElement>("[data-diff]")!;

    expect(classes(comparison)).toEqual(classesOf(diff().root()));
    expect(classes(comparison)).toContain("border-muted");
    expect(comparison.innerHTML).toContain("var(--reddb-spatial-inset-md)");
    expect(
      scope.matches(
        '[data-theme="application"][data-color-scheme="dark"][data-density="compact"][data-motion="reduced"]',
      ),
    ).toBe(true);
    for (const axis of ["data-theme", "data-color-scheme", "data-density", "data-motion"]) {
      expect(comparison.hasAttribute(axis)).toBe(false);
    }
  });

  it("merges caller classes and passes native attributes", () => {
    const comparison = rendered(
      render(Diff, { rows: ROWS, id: "release-diff", class: "shadow-sm" }),
    );

    expect(comparison.id).toBe("release-diff");
    expect(classes(comparison).has("shadow-sm")).toBe(true);
    expect(classes(comparison).has("border-muted")).toBe(true);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("diff", "Diff");
  });
});
