import { describe, expect, it, vi } from "vitest";
import StatusVocabularyConsumer from "./fixtures/StatusVocabularyConsumer.svelte";
import StatusVocabularyContractFailures from "./fixtures/StatusVocabularyContractFailures.svelte";
import {
  STATUS_INDICATOR_STATUSES,
  STATUS_INDICATOR_TONES,
  StatusIndicator,
  TONES,
  statusIndicator,
} from "./fixtures/status-vocabulary-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function statusMeaningFailures(root: HTMLElement): string[] {
  const hasMark = root.querySelector("[data-status-mark]") !== null;
  const label = root.querySelector("[data-status-label]")?.textContent?.trim();
  const accessibleName = root.getAttribute("aria-label")?.trim();
  return hasMark && !label && !accessibleName
    ? ["status is conveyed by colour alone"]
    : [];
}

describe("the Base StatusIndicator", () => {
  it("is available with its status vocabulary and Extension Seam through Base", () => {
    expect(StatusIndicator).toBeDefined();
    expect(statusIndicator).toBeTypeOf("function");
    // Meaning is the shared tone vocabulary (ADR 0026); the status list is a
    // deprecated name for the same list.
    expect(STATUS_INDICATOR_TONES).toEqual(TONES);
    expect(STATUS_INDICATOR_STATUSES).toEqual(TONES);
  });

  it("keeps the deprecated status prop as a working alias that warns once in development", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const elements = [1, 2].map(() => rendered(render(StatusIndicator, { label: "Down", status: "danger" })));
      for (const element of elements) {
        expect(element.dataset.tone).toBe("danger");
        expect(classes(element.querySelector("[data-status-mark]")!)).toEqual(
          classesOf(statusIndicator({ tone: "danger" }).mark()),
        );
      }
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]![0])).toMatch(/\[reddb StatusIndicator\] status="danger" is deprecated.*tone="danger"/);
    } finally {
      warn.mockRestore();
    }
  });

  it("always names every status in text, even when the label is visually hidden", () => {
    for (const status of STATUS_INDICATOR_TONES) {
      const visible = rendered(render(StatusIndicator, { label: `${status} status`, tone: status }));
      const hidden = rendered(render(StatusIndicator, {
        label: `${status} status`,
        tone: status,
        showLabel: false,
      }));

      expect(visible.textContent).toContain(`${status} status`);
      expect(visible.getAttribute("data-status")).toBe(status);
      expect(classes(hidden.querySelector("[data-status-label]")!).has("sr-only")).toBe(true);
      expect(hidden.querySelector("[data-status-mark]")?.getAttribute("aria-hidden")).toBe("true");
      expect(statusMeaningFailures(hidden)).toEqual([]);
    }
  });

  it("does not become a control while preserving native attributes", () => {
    const element = rendered(render(StatusIndicator, {
      label: "Available",
      id: "availability",
      title: "Current availability",
    }));
    expect(element.tabIndex).toBe(-1);
    expect(element.id).toBe("availability");
    expect(element.title).toBe("Current availability");
  });

  it("wears token-backed appearance and inherits every nested appearance axis", () => {
    const element = rendered(render(StatusIndicator, {
      label: "Delayed",
      tone: "warning",
      class: "max-w-40",
    }));
    const nested = rendered(render(StatusVocabularyConsumer, {}))
      .querySelector<HTMLElement>("[data-status-indicator]")!;
    const slots = statusIndicator({ tone: "warning" });

    expect(classes(element)).toEqual(classesOf(slots.root({ class: "max-w-40" })));
    expect(classes(element.querySelector("[data-status-mark]")!)).toEqual(classesOf(slots.mark()));
    for (const root of [element, nested]) {
      expect(root.hasAttribute("data-theme")).toBe(false);
      expect(root.hasAttribute("data-color-scheme")).toBe(false);
      expect(root.hasAttribute("data-density")).toBe(false);
    }
  });
});

describe("the deliberately failing status fixture", () => {
  it("diagnoses a status conveyed by colour alone", () => {
    const root = rendered(render(StatusVocabularyContractFailures, {}));
    expect(statusMeaningFailures(root)).toEqual(["status is conveyed by colour alone"]);
  });
});
