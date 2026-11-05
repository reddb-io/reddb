import { createRawSnippet, type Snippet } from "svelte";
import { describe, expect, it } from "vitest";
import NotificationEmptyStateContractFailures from "./fixtures/NotificationEmptyStateContractFailures.svelte";
import NotificationEmptyStateConsumer from "./fixtures/NotificationEmptyStateConsumer.svelte";
import {
  EMPTY_STATE_SIZES,
  EmptyState,
  emptyState,
} from "./fixtures/notification-empty-state-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function content(markup: string): Snippet {
  return createRawSnippet(() => ({ render: () => markup }));
}

describe("the deliberately unnamed empty-state fixture", () => {
  it("demonstrates an action with no sentence saying what is empty", () => {
    const empty = rendered(
      render(NotificationEmptyStateContractFailures, { failure: "unnamed-empty-state" }),
    );

    expect(empty.querySelector("button")?.textContent).toBe("Add a node");
    expect(empty.querySelector("p")).toBeNull();
  });
});

describe("the Base EmptyState", () => {
  it("renders the required sentence that names the empty region", () => {
    const empty = rendered(render(EmptyState, { title: "No nodes yet" }));

    expect(empty.textContent?.trim()).toBe("No nodes yet");
  });

  it("publishes complete size and border appearance without fixing an axis", () => {
    expect(EMPTY_STATE_SIZES).toEqual(["sm", "md"]);

    for (const size of EMPTY_STATE_SIZES) {
      for (const bordered of [true, false]) {
        const empty = rendered(render(EmptyState, { title: "Empty", size, bordered }));
        expect(classes(empty)).toEqual(classesOf(emptyState({ size, bordered }).root()));
        expect([...classes(empty)].join(" ")).toContain("var(--reddb-spatial-");
        expect(empty.hasAttribute("data-theme")).toBe(false);
        expect(empty.hasAttribute("data-color-scheme")).toBe(false);
        expect(empty.hasAttribute("data-density")).toBe(false);
      }
    }
  });

  it("renders optional supporting copy only when the caller supplies it", () => {
    const supported = rendered(
      render(EmptyState, {
        title: "No nodes yet",
        description: "A node appears here as soon as one joins.",
      }),
    );
    const bare = rendered(render(EmptyState, { title: "No nodes yet" }));

    expect([...supported.querySelectorAll("p")].map(({ textContent }) => textContent)).toEqual([
      "No nodes yet",
      "A node appears here as soon as one joins.",
    ]);
    expect(bare.querySelectorAll("p")).toHaveLength(1);
  });

  it("keeps caller media decorative because the required title carries the message", () => {
    const empty = rendered(
      render(EmptyState, { title: "No nodes yet", media: content("<span>Empty box</span>") }),
    );
    const media = empty.firstElementChild!;

    expect(media.getAttribute("aria-hidden")).toBe("true");
    expect(classes(media)).toEqual(classesOf(emptyState({}).media()));
  });

  it("renders a literal hint between its explanation and caller-owned actions", () => {
    const empty = rendered(
      render(EmptyState, {
        title: "No nodes yet",
        description: "A node appears here as soon as one joins.",
        hint: "reddb node add --name alpha",
        actions: content('<button type="button">Add a node</button>'),
      }),
    );
    const hint = empty.querySelector("code")!;
    const children = [...empty.children];

    expect(hint.textContent).toBe("reddb node add --name alpha");
    expect(children.indexOf(hint)).toBeGreaterThan(children.indexOf(empty.querySelectorAll("p")[1]!));
    expect(children.indexOf(hint)).toBeLessThan(children.length - 1);
  });

  it("preserves the caller's native action and keyboard focus order", () => {
    const empty = rendered(
      render(EmptyState, {
        title: "No nodes yet",
        actions: content('<button type="button">Add a node</button>'),
      }),
    );
    const action = empty.querySelector<HTMLButtonElement>("button")!;

    action.focus();
    expect(document.activeElement).toBe(action);
    expect(action.textContent).toBe("Add a node");
    expect(action.closest("div")?.parentElement).toBe(empty);
  });

  it("merges caller classes and forwards native attributes", () => {
    const empty = rendered(
      render(EmptyState, { title: "No nodes yet", id: "nodes-empty", class: "max-w-lg" }),
    );

    expect(empty.id).toBe("nodes-empty");
    expect(classes(empty)).toEqual(
      classesOf(emptyState({ size: "md", bordered: true }).root({ class: "max-w-lg" })),
    );
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const nested = render(NotificationEmptyStateConsumer)
      .querySelector<HTMLElement>("[data-nested-empty-state] > div")!;

    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});
