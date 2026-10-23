import { createRawSnippet, type Snippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import { ALERT_FEEDBACK_ROLES, ALERT_TONES, Alert, FEEDBACK_TONES, TONES, alert } from "./fixtures/alert-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function text(content: string): Snippet {
  return createRawSnippet(() => ({ render: () => `<span>${content}</span>` }));
}

describe("the Base Alert", () => {
  it("is available with its tone appearance seam through Base", () => {
    expect(Alert).toBeDefined();
    expect(alert).toBeTypeOf("function");
    // Meaning is the shared tone vocabulary (ADR 0026); the old Feedback Role
    // list stays one release as the four tones that name a role.
    expect(ALERT_TONES).toEqual(TONES);
    expect(ALERT_FEEDBACK_ROLES).toEqual(FEEDBACK_TONES);
  });

  it("announces neutral, info, success, and warning as status, and danger as an error alert", () => {
    for (const tone of TONES) {
      const element = rendered(render(Alert, { tone, children: text("Deployment changed") }));
      expect(element.getAttribute("role")).toBe(tone === "danger" ? "alert" : "status");
      expect(element.dataset.tone).toBe(tone);
    }
    expect(rendered(render(Alert, { children: text("Plain") })).dataset.tone).toBe("neutral");
  });

  it("renders an optional title and caller-owned detail", () => {
    const element = rendered(
      render(Alert, {
        tone: "success",
        title: "Deployment complete",
        children: text("The new release is live."),
      }),
    );
    expect(element.textContent).toContain("Deployment complete");
    expect(element.textContent).toContain("The new release is live.");
  });

  it("wears only stable Feedback Role names under every tone", () => {
    for (const tone of FEEDBACK_TONES) {
      const element = rendered(render(Alert, { tone }));
      expect(classes(element)).toEqual(classesOf(alert({ tone })));
      expect([...classes(element)].join(" ")).toContain(`feedback-${tone}-surface`);
      expect([...classes(element)].join(" ")).not.toMatch(/(?:red|green|yellow|neutral)-\d/);
    }
    expect(classes(rendered(render(Alert, { tone: "neutral" })))).toEqual(
      classesOf("rounded-md border px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] text-sm border-transparent bg-muted text-foreground"),
    );
  });

  it("forwards native attributes and merges a caller class", () => {
    const element = rendered(
      render(Alert, { tone: "warning", id: "quota-warning", class: "mt-4" }),
    );
    expect(element.id).toBe("quota-warning");
    expect(classes(element)).toEqual(classesOf(alert({ tone: "warning", class: "mt-4" })));
  });

  it("keeps the deprecated feedback prop as a working alias that warns once in development", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const elements = [1, 2].map(() => rendered(render(Alert, { feedback: "danger" })));
      for (const element of elements) {
        expect(element.getAttribute("role")).toBe("alert");
        expect(classes(element)).toEqual(classesOf(alert({ tone: "danger" })));
      }
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]![0])).toMatch(/\[reddb Alert\] feedback="danger" is deprecated.*tone="danger"/);
    } finally {
      warn.mockRestore();
    }
  });
});
