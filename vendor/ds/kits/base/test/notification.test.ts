import { createRawSnippet, flushSync, type Snippet } from "svelte";
import { describe, expect, it } from "vitest";
import NotificationEmptyStateContractFailures from "./fixtures/NotificationEmptyStateContractFailures.svelte";
import NotificationEmptyStateConsumer from "./fixtures/NotificationEmptyStateConsumer.svelte";
import { Notification, notification } from "./fixtures/notification-empty-state-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function text(content: string): Snippet {
  return createRawSnippet(() => ({ render: () => `<span>${content}</span>` }));
}

describe("the deliberately silent notification fixture", () => {
  it("demonstrates a visible notification that never reaches assistive technology", () => {
    const notification = rendered(
      render(NotificationEmptyStateContractFailures, { failure: "silent-notification" }),
    );

    expect(notification.textContent).toContain("Deployment complete");
    expect(notification.getAttribute("role")).toBeNull();
    expect(notification.getAttribute("aria-live")).toBeNull();
  });
});

describe("the Base Notification", () => {
  it("announces transient feedback through the canonical Alert contract", () => {
    const root = rendered(
      render(Notification, {
        feedback: "success",
        title: "Deployment complete",
        children: text("The new release is live."),
      }),
    );

    const announcement = root.querySelector<HTMLElement>("[role=status]");
    expect(announcement?.textContent).toContain("Deployment complete");
    expect(announcement?.textContent).toContain("The new release is live.");
  });

  it("keeps Alert's assertive urgency for danger feedback", () => {
    const root = rendered(render(Notification, { feedback: "danger", title: "Deployment failed" }));

    expect(root.querySelector("[role=alert]")?.textContent).toContain("Deployment failed");
    expect(root.querySelector("[role=status]")).toBeNull();
  });

  it("dismisses from a native keyboard-focusable Button and notifies the caller", () => {
    let dismissals = 0;
    const host = render(Notification, {
      feedback: "warning",
      title: "Capacity is nearly full",
      dismissLabel: "Dismiss capacity warning",
      ondismiss: () => dismissals++,
    });
    const dismiss = host.querySelector<HTMLButtonElement>("button")!;

    expect(dismiss.type).toBe("button");
    expect(dismiss.getAttribute("aria-label")).toBe("Dismiss capacity warning");
    dismiss.focus();
    expect(document.activeElement).toBe(dismiss);
    dismiss.click();
    flushSync();

    expect(host.querySelector("[data-notification]")).toBeNull();
    expect(dismissals).toBe(1);
  });

  it("wears its exported Density seam without selecting an appearance axis", () => {
    const root = rendered(
      render(Notification, {
        feedback: "success",
        title: "Deployment complete",
        class: "mt-4",
        id: "deployment-notification",
      }),
    );
    const slots = notification();

    expect(root.id).toBe("deployment-notification");
    expect(classes(root)).toEqual(classesOf(slots.root({ class: "mt-4" })));
    expect(classes(root).has("gap-[var(--reddb-spatial-gap-sm)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const nested = render(NotificationEmptyStateConsumer)
      .querySelector<HTMLElement>("[data-nested-notification] > [data-notification]")!;

    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});
